use super::*;

impl D3d12Compositor {
    pub(crate) fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        if !self.mode.accepts(connection) {
            return Err(
                format!("connection {connection} does not belong to {:?}", self.mode).into(),
            );
        }
        self.disconnect_stream(connection)?;
        if FIRST_CONTENT_DESCRIPTOR + (self.surfaces.len() as u32 + 1) * SURFACE_DESCRIPTORS
            > DESCRIPTOR_COUNT
        {
            return Err("D3D12 surface descriptor heap is full".into());
        }
        let imported = self.receiver.import_stream(stream)?;
        self.surfaces.push(ImportedSurface::new(
            self.receiver.device(),
            imported,
            stream,
            connection,
            quarter_turns,
        )?);
        self.surfaces
            .sort_by_key(|surface| polymer_renderer::surface_sort_key(surface.surface_id));
        self.refresh_surface_descriptors()?;
        Ok(())
    }

    pub(crate) fn set_buffer_release_sender(&mut self, offer: &polymer_runtime::SurfaceOffer) {
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.connection == offer.connection_id())
        {
            surface.frames.release_sender = offer.buffer_release_sender();
        }
    }

    pub(crate) fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.connection == connection)
        else {
            return;
        };
        if surface.imported.resource(frame.slot).is_none() {
            return;
        }
        surface.frames.offer(frame);
    }

    pub fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        self.surfaces
            .iter()
            .filter_map(|surface| {
                let frame = surface.frames.latest()?;
                let image_extent = polymer_renderer::renderer::frame_image_extent(
                    frame,
                    surface.imported.width,
                    surface.imported.height,
                    surface.quarter_turns,
                );
                Some(ReceivedSurface {
                    connection: surface.connection,
                    surface_id: surface.surface_id,
                    target_window: surface.target_window,
                    image_extent,
                    quarter_turns: surface.quarter_turns,
                    flip_y: surface.flip_y,
                    frame,
                })
            })
            .collect()
    }

    pub(crate) fn has_pending_frame(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| surface.frames.has_pending())
    }

    pub fn wait_for_ready_pending_frame(&self, timeout: Duration) -> windows::core::Result<bool> {
        for surface in &self.surfaces {
            if let Some(frame) = surface.frames.pending() {
                return surface
                    .imported
                    .wait_until_producer_ready(frame.sync_value, timeout);
            }
        }
        Ok(false)
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
                .signal_release(self.receiver.queue(), RELEASE_ALL_SYNC_VALUE)?;
            self.wait_idle()?;
            self.refresh_surface_descriptors()?;
        }
        Ok(())
    }

    pub(super) fn release_imported_streams(&mut self) -> windows::core::Result<()> {
        for surface in &self.surfaces {
            surface
                .imported
                .signal_release(self.receiver.queue(), RELEASE_ALL_SYNC_VALUE)?;
        }
        self.wait_idle()?;
        self.surfaces.clear();
        Ok(())
    }

    pub(super) fn refresh_surface_descriptors(&mut self) -> windows::core::Result<()> {
        // Sorting or removing a surface moves descriptor ranges. Retire every
        // draw using the old ranges before writing any descriptors.
        self.wait_idle()?;
        for (index, surface) in self.surfaces.iter_mut().enumerate() {
            surface.descriptor = FIRST_CONTENT_DESCRIPTOR + index as u32 * SURFACE_DESCRIPTORS;
            surface.write_descriptors(self.receiver.device(), &self.srv_heap, self.srv_stride);
        }
        Ok(())
    }
}
