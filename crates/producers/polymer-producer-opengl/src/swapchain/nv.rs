use super::*;

impl NvCapture {
    #[expect(clippy::too_many_arguments)]
    pub(super) unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        bottom_up: bool,
        connection: &mut Option<ProducerSession>,
    ) -> Result<Self, String> {
        let transport = unsafe { nv_dx::Ring::new(width, height) }?;
        let mut framebuffer = 0;
        let mut read_framebuffer = 0;
        unsafe {
            (transport.functions.gen_framebuffers)(1, &mut framebuffer);
            (transport.functions.gen_framebuffers)(1, &mut read_framebuffer);
        }
        let publication = Self::publish(
            &transport,
            surface_id,
            source_window,
            width,
            height,
            bottom_up,
            connection.take().expect("negotiated connection"),
        );
        tracing::debug!(
            "polymer: attached WGL_NV_DX_interop ring {width}x{height} (surface {:#x})",
            surface_id.0
        );
        let mut capture = Self {
            width,
            height,
            window_width,
            window_height,
            framebuffer,
            read_framebuffer,
            transport,
            ipc: None,
            scheduler: Default::default(),
            frame_number: 0,
        };
        capture.ipc =
            ProducerSession::setup_result(publication).map_err(|error| error.to_string())?;
        Ok(capture)
    }

    pub(super) unsafe fn copy_frame(
        &mut self,
        stream_id: StreamId,
        source: Option<&DrawCandidate>,
    ) -> Result<Option<()>, String> {
        let count = self.transport.slots.len();
        let Some(reservation) = self
            .scheduler
            .try_acquire(count, |index| unsafe { self.transport.acquire(index) }.ok())
        else {
            return Ok(None);
        };
        let slot_index = reservation.0;
        let slot = &self.transport.slots[slot_index];
        let f = self.transport.functions;
        let mut old_read = 0;
        let mut old_draw = 0;
        let scissor_enabled = unsafe { glIsEnabled(GL_SCISSOR_TEST) } != 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut old_read);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut old_draw);
            (f.bind_framebuffer)(
                GL_READ_FRAMEBUFFER,
                if source.is_some_and(|source| source.texture != 0) {
                    self.read_framebuffer
                } else {
                    0
                },
            );
            (f.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.framebuffer);
            (f.framebuffer_texture_2d)(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                slot.texture,
                0,
            );
            if scissor_enabled {
                glDisable(GL_SCISSOR_TEST);
            }
            if let Some(source) = source {
                source.attach_read_framebuffer(
                    f.framebuffer_texture_2d,
                    f.framebuffer_texture_layer,
                    f.framebuffer_renderbuffer,
                );
                (f.blit_framebuffer)(
                    source.source_origin.0,
                    source.source_origin.1,
                    source.source_origin.0 + source.width as i32,
                    source.source_origin.1 + source.height as i32,
                    0,
                    0,
                    source.width as i32,
                    self.height as i32,
                    GL_COLOR_BUFFER_BIT,
                    GL_NEAREST,
                );
            } else {
                (f.blit_framebuffer)(
                    0,
                    0,
                    self.width as i32,
                    self.height as i32,
                    0,
                    0,
                    self.width as i32,
                    self.height as i32,
                    GL_COLOR_BUFFER_BIT,
                    GL_NEAREST,
                );
            }
            if scissor_enabled {
                glEnable(GL_SCISSOR_TEST);
            }
            (f.bind_framebuffer)(GL_READ_FRAMEBUFFER, old_read as u32);
            (f.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, old_draw as u32);
        }
        reservation.1.finish()?;
        self.scheduler.commit(slot_index, count);
        self.frame_number = self.frame_number.wrapping_add(1);
        let message = polymer_presentation::producer::CapturedFrame {
            stream_id,
            slot: slot_index as u32,
            sync_value: self.frame_number,
            timestamp_qpc: performance_counter().unwrap_or(0),
            extent: polymer_presentation::Extent2D {
                width: self.width,
                height: self.height,
            },
        }
        .present(DrawCandidate::frame_placement(
            source,
            polymer_presentation::Extent2D {
                width: self.window_width,
                height: self.window_height,
            },
        ));
        ProducerSession::publish_frame(&mut self.ipc, message);
        Ok(Some(()))
    }
}

impl Drop for NvCapture {
    fn drop(&mut self) {
        unsafe {
            (self.transport.functions.delete_framebuffers)(1, &self.framebuffer);
            (self.transport.functions.delete_framebuffers)(1, &self.read_framebuffer);
        }
    }
}

impl NvCapture {
    fn publish(
        transport: &nv_dx::Ring,
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        bottom_up: bool,
        mut socket: ProducerSession,
    ) -> std::io::Result<ProducerSession> {
        let image_handles = transport
            .ring
            .slots()
            .iter()
            .map(|slot| slot.raw_handle() as isize as i64)
            .collect::<Vec<_>>();
        socket.publish(ExportDescriptor {
            surface_id,
            adapter_luid: transport.adapter_luid,
            source_window: source_window as i64,
            width,
            height,
            format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
            image_kind: ExternalImageKind::D3d11SharedResourceNt,
            sync_kind: ExternalSyncKind::D3d11KeyedMutex,
            image_handles: &image_handles,
            timeline_semaphore_handle: 0,
            consumer_semaphore_handle: 0,
            bottom_up,
        })?;
        Ok(socket)
    }
}
