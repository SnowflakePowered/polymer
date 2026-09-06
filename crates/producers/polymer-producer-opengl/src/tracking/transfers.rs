use super::*;

impl ContextTracking {
    /// Follows texture-to-texture copies through the tracking graph. Copies are
    /// how display-transfer emulation moves a finished frame into the surface the
    /// presenter samples, so the destination is a render target fed by the source.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors copy-image source and destination arguments"
    )]
    pub unsafe fn record_copy_candidate(
        &self,
        src_name: Gluint,
        src_target: Glenum,
        source: [i32; 4],
        dst_name: Gluint,
        dst_target: Glenum,
        destination: [i32; 4],
        extent: [i32; 3],
    ) {
        if polymer_hook::config::tracked_render_target_count().is_none()
            || extent.iter().any(|value| *value <= 0)
        {
            return;
        }
        let context = unsafe { current_context() } as usize;
        let order = self.observation_order.fetch_add(1, Ordering::Relaxed);
        for layer in 0..extent[2] {
            let Some(source_candidate) = (unsafe {
                self.texture_candidate(src_name, src_target, source[0], source[3] + layer)
            }) else {
                continue;
            };
            let Some(destination_candidate) = (unsafe {
                self.texture_candidate(dst_name, dst_target, destination[0], destination[3] + layer)
            }) else {
                continue;
            };
            let size = polymer_presentation::Extent2D {
                width: extent[0] as u32,
                height: extent[1] as u32,
            };
            let mapping = polymer_presentation::RegionMapping {
                source: polymer_presentation::Rect {
                    x: source[1],
                    y: source[2],
                    extent: size,
                },
                destination: polymer_presentation::Rect {
                    x: destination[1],
                    y: destination[2],
                    extent: size,
                },
                flip_x: false,
                flip_y: false,
            };
            let mut edge = polymer_presentation::DependencyEdge::sampled(
                source_candidate.key(),
                destination_candidate.key(),
                self.generation(),
            );
            edge.order = order;
            edge.evidence = polymer_presentation::Evidence::Copy;
            edge.mapping = Some(mapping);
            edge.coverage = Some(mapping.destination);
            self.upsert_render_target(source_candidate);
            self.upsert_render_target(destination_candidate);
            self.observations.entry(context).or_default().record(edge);
        }
    }

    unsafe fn texture_candidate(
        &self,
        texture: Gluint,
        target: Glenum,
        mip: i32,
        layer: i32,
    ) -> Option<DrawCandidate> {
        if target == GL_RENDERBUFFER {
            return (mip == 0 && layer == 0)
                .then(|| unsafe { self.renderbuffer_candidate(texture) })
                .flatten();
        }
        if texture == 0 || mip < 0 || layer < 0 {
            return None;
        }
        let (width, height) = unsafe { self.measure_texture(texture, target) }?;
        let width = width.checked_shr(mip as u32)?.max(1);
        let height = height.checked_shr(mip as u32)?.max(1);
        let target = self
            .texture_info
            .get(&texture)
            .map_or(target, |info| info.0);
        Some(DrawCandidate {
            mip,
            incarnation: self.incarnation(false, texture),
            orientation: (false, false),
            source_origin: (0, 0),
            allocation_extent: None,
            texture,
            target,
            layer,
            width,
            height,
            bounds: QuadBounds {
                x: 0,
                y: 0,
                width,
                height,
            },
            window_bounds: false,
            score: 0,
        })
    }

    unsafe fn framebuffer_candidate(
        &self,
        target: Glenum,
        attachment: Glenum,
    ) -> Option<DrawCandidate> {
        if attachment < crate::interop::GL_COLOR_ATTACHMENT0 {
            return None;
        }
        let query = self.framebuffer_attachment_query()?;
        let mut kind = 0;
        let mut name = 0;
        let mut mip = 0;
        let mut layer = 0;
        unsafe {
            query(
                target,
                attachment,
                GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
                &mut kind,
            );
            query(
                target,
                attachment,
                GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
                &mut name,
            );
            if kind == GL_TEXTURE as i32 {
                query(
                    target,
                    attachment,
                    GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL,
                    &mut mip,
                );
                query(
                    target,
                    attachment,
                    GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER,
                    &mut layer,
                );
            }
        }
        let resource_target = match kind as u32 {
            GL_TEXTURE => GL_TEXTURE_2D,
            GL_RENDERBUFFER => GL_RENDERBUFFER,
            _ => return None,
        };
        unsafe { self.texture_candidate(name as u32, resource_target, mip, layer) }
    }

    unsafe fn blit_destinations(&self, framebuffer: i32) -> Vec<TargetKey> {
        let mut count = 0;
        unsafe { glGetIntegerv(GL_MAX_DRAW_BUFFERS, &mut count) };
        let mut destinations = Vec::new();
        for index in 0..count.clamp(1, 16) {
            let mut attachment = 0;
            unsafe { glGetIntegerv(GL_DRAW_BUFFER0 + index as u32, &mut attachment) };
            if attachment == 0 {
                continue;
            }
            if framebuffer == 0 {
                destinations.push(SelectionView::PRESENT);
                break;
            }
            if let Some(candidate) =
                unsafe { self.framebuffer_candidate(GL_DRAW_FRAMEBUFFER, attachment as u32) }
            {
                self.upsert_render_target(candidate);
                if !destinations.contains(&candidate.key()) {
                    destinations.push(candidate.key());
                }
            }
        }
        destinations
    }

    /// Follows framebuffer blits through the tracking graph. Blits into an
    /// offscreen framebuffer make the destination a render target fed by the
    /// source (compositors and display-transfer emulation copy screens this way),
    /// and a blit into the default framebuffer is a presentation root, since no
    /// default-framebuffer draw exists to observe.
    pub unsafe fn record_blit_candidate(
        &self,
        source_bounds: [i32; 4],
        destination_bounds: [i32; 4],
        mask: u32,
    ) {
        if mask & crate::interop::GL_COLOR_BUFFER_BIT == 0 {
            return;
        }
        if polymer_hook::config::tracked_render_target_count().is_none() {
            return;
        }
        let context = unsafe { current_context() };
        if context.is_null() {
            return;
        }
        let mut read_framebuffer = 0;
        let mut draw_framebuffer = 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut read_framebuffer);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut draw_framebuffer);
        }
        if read_framebuffer == 0 {
            return;
        }
        let mut read_buffer = 0;
        unsafe { glGetIntegerv(0x0C02, &mut read_buffer) };
        let Some(source) =
            (unsafe { self.framebuffer_candidate(GL_READ_FRAMEBUFFER, read_buffer as u32) })
        else {
            return;
        };
        self.upsert_render_target(source);
        let destinations = unsafe { self.blit_destinations(draw_framebuffer) };
        let rect = |[x0, y0, x1, y1]: [i32; 4]| polymer_presentation::Rect {
            x: x0.min(x1),
            y: y0.min(y1),
            extent: polymer_presentation::Extent2D {
                width: x0.abs_diff(x1),
                height: y0.abs_diff(y1),
            },
        };
        let mapping = polymer_presentation::RegionMapping {
            source: rect(source_bounds),
            destination: rect(destination_bounds),
            flip_x: (source_bounds[0] > source_bounds[2])
                != (destination_bounds[0] > destination_bounds[2]),
            flip_y: (source_bounds[1] > source_bounds[3])
                != (destination_bounds[1] > destination_bounds[3]),
        };
        let order = self.observation_order.fetch_add(1, Ordering::Relaxed);
        for destination in destinations {
            let mut edge = polymer_presentation::DependencyEdge::sampled(
                source.key(),
                destination,
                self.generation(),
            );
            edge.order = order;
            edge.evidence = polymer_presentation::Evidence::Blit;
            edge.mapping = Some(mapping);
            edge.coverage = Some(mapping.destination);
            self.observations
                .entry(context as usize)
                .or_default()
                .record(edge);
        }
    }
}
