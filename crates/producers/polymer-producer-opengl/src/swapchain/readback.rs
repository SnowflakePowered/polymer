use super::*;

impl PboCapture {
    #[expect(clippy::too_many_arguments)]
    pub(super) unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        _whole_window: bool,
        connection: &mut Option<ProducerSession>,
    ) -> Result<Self, String> {
        let transport = unsafe { pbo::Transport::new(width, height) }?;
        let gen_framebuffers: crate::interop::GlGenFramebuffers =
            unsafe { crate::interop::proc(b"glGenFramebuffers\0") }
                .ok_or("missing glGenFramebuffers")?;
        let bind_framebuffer = unsafe { crate::interop::proc(b"glBindFramebuffer\0") }
            .ok_or("missing glBindFramebuffer")?;
        let framebuffer_texture_2d = unsafe { crate::interop::proc(b"glFramebufferTexture2D\0") }
            .ok_or("missing glFramebufferTexture2D")?;
        let framebuffer_renderbuffer =
            unsafe { crate::interop::proc(b"glFramebufferRenderbuffer\0") }
                .ok_or("missing glFramebufferRenderbuffer")?;
        let framebuffer_texture_layer =
            unsafe { crate::interop::proc(b"glFramebufferTextureLayer\0") }
                .ok_or("missing glFramebufferTextureLayer")?;
        let delete_framebuffers = unsafe { crate::interop::proc(b"glDeleteFramebuffers\0") }
            .ok_or("missing glDeleteFramebuffers")?;
        let mut read_framebuffer = 0;
        unsafe { gen_framebuffers(1, &mut read_framebuffer) };
        let publication = Self::publish(
            &transport,
            surface_id,
            source_window,
            width,
            height,
            connection.take().expect("negotiated connection"),
        );
        tracing::debug!(
            "polymer: attached asynchronous OpenGL PBO ring {width}x{height} (surface {:#x})",
            surface_id.0
        );
        let mut capture = Self {
            width,
            height,
            window_width,
            window_height,
            read_framebuffer,
            bind_framebuffer,
            framebuffer_texture_2d,
            framebuffer_texture_layer,
            framebuffer_renderbuffer,
            delete_framebuffers,
            transport,
            ipc: None,
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
        let mut old_read = 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut old_read);
            (self.bind_framebuffer)(
                GL_READ_FRAMEBUFFER,
                if source.is_some_and(|source| source.texture != 0) {
                    self.read_framebuffer
                } else {
                    0
                },
            );
            if let Some(source) = source {
                source.attach_read_framebuffer(
                    self.framebuffer_texture_2d,
                    self.framebuffer_texture_layer,
                    self.framebuffer_renderbuffer,
                );
            }
        }
        let result = unsafe {
            self.transport.transfer(
                source.map_or(0, |source| source.source_origin.0),
                source.map_or(0, |source| source.source_origin.1),
                DrawCandidate::frame_placement(
                    source,
                    polymer_presentation::Extent2D {
                        width: self.window_width,
                        height: self.window_height,
                    },
                ),
            )
        };
        unsafe { (self.bind_framebuffer)(GL_READ_FRAMEBUFFER, old_read as u32) };
        let Some((slot_index, timestamp_qpc, placement)) = result? else {
            return Ok(None);
        };
        self.frame_number = self.frame_number.wrapping_add(1);
        let message = polymer_presentation::producer::CapturedFrame {
            stream_id,
            slot: slot_index as u32,
            sync_value: self.frame_number,
            timestamp_qpc,
            extent: polymer_presentation::Extent2D {
                width: self.width,
                height: self.height,
            },
        }
        .present(placement);
        ProducerSession::publish_frame(&mut self.ipc, message);
        Ok(Some(()))
    }
}

impl Drop for PboCapture {
    fn drop(&mut self) {
        unsafe { (self.delete_framebuffers)(1, &self.read_framebuffer) }
    }
}

impl PboCapture {
    fn publish(
        transport: &pbo::Transport,
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
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
            // glReadPixels starts at the framebuffer's bottom row.
            bottom_up: true,
        })?;
        Ok(socket)
    }
}
