use super::*;

impl ExtCapture {
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
        let functions = unsafe { GlFunctions::load() }
            .ok_or("GL_EXT_memory_object_win32 and GL_EXT_semaphore_win32 are unavailable")?;
        let mut luid_bytes = [0u8; 8];
        unsafe { (functions.get_unsigned_byte)(GL_DEVICE_LUID_EXT, luid_bytes.as_mut_ptr()) };
        let adapter_luid = AdapterLuid {
            low_part: u32::from_le_bytes(luid_bytes[..4].try_into().expect("four bytes")),
            high_part: i32::from_le_bytes(luid_bytes[4..].try_into().expect("four bytes")),
        };
        let (device, adapter) = unsafe { ExternalImage::create_device(adapter_luid) }?;
        let adapter_desc = unsafe { adapter.GetDesc1() }
            .map_err(|error| format!("IDXGIAdapter1::GetDesc1: {error}"))?;
        let adapter_luid = AdapterLuid {
            low_part: adapter_desc.AdapterLuid.LowPart,
            high_part: adapter_desc.AdapterLuid.HighPart,
        };
        let mut slots = Vec::with_capacity(3);
        for _ in 0..3 {
            slots.push(ImageSlot {
                image: unsafe { ExternalImage::new(&device, &functions, width, height) }?,
                release_value: AtomicU64::new(0),
            });
        }
        let ready = unsafe { Timeline::new(&device, &functions) }?;
        let consumer = unsafe { Timeline::new(&device, &functions) }?;
        let mut framebuffer = 0;
        let mut read_framebuffer = 0;
        unsafe {
            (functions.gen_framebuffers)(1, &mut framebuffer);
            (functions.gen_framebuffers)(1, &mut read_framebuffer);
        }
        let publication = Self::publish(
            &slots,
            ready.handle(),
            consumer.handle(),
            adapter_luid,
            surface_id,
            source_window,
            width,
            height,
            bottom_up,
            connection.take().expect("negotiated connection"),
        );
        tracing::debug!(
            "polymer: attached OpenGL EXT external-object ring {width}x{height} (surface {:#x}, window {source_window:#x} {window_width}x{window_height})",
            surface_id.0
        );
        let mut capture = Self {
            width,
            height,
            window_width,
            window_height,
            functions,
            framebuffer,
            read_framebuffer,
            slots,
            _device: device,
            ready,
            consumer,
            ipc: None,
            scheduler: Default::default(),
            sync_value: 0,
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
        let completed = unsafe { self.consumer.completed_value() };
        let sync_value = self.sync_value.wrapping_add(1).max(1);
        let Some(reservation) = self.scheduler.try_acquire(self.slots.len(), |index| {
            (self.slots[index].release_value.load(Ordering::Relaxed) <= completed).then_some(())
        }) else {
            return Ok(None);
        };
        let slot_index = reservation.0;
        let slot = &self.slots[slot_index];
        let mut old_read = 0;
        let mut old_draw = 0;
        let scissor_enabled = unsafe { glIsEnabled(GL_SCISSOR_TEST) } != 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut old_read);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut old_draw);
            if source.is_some_and(|source| source.texture != 0) {
                (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, self.read_framebuffer);
            } else {
                (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, 0);
            }
            (self.functions.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.framebuffer);
            (self.functions.framebuffer_texture_2d)(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                slot.image.texture,
                0,
            );
            if scissor_enabled {
                glDisable(GL_SCISSOR_TEST);
            }

            match source {
                None => {
                    (self.functions.blit_framebuffer)(
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
                Some(source) => {
                    source.attach_read_framebuffer(
                        self.functions.framebuffer_texture_2d,
                        self.functions.framebuffer_texture_layer,
                        self.functions.framebuffer_renderbuffer,
                    );
                    (self.functions.blit_framebuffer)(
                        source.selection.source_rect.x,
                        source.selection.source_rect.y,
                        source.selection.source_rect.x
                            + source.selection.source_rect.extent.width as i32,
                        source.selection.source_rect.y
                            + source.selection.source_rect.extent.height as i32,
                        0,
                        0,
                        source.selection.source_rect.extent.width as i32,
                        self.height as i32,
                        GL_COLOR_BUFFER_BIT,
                        GL_NEAREST,
                    );
                }
            }
            if scissor_enabled {
                glEnable(GL_SCISSOR_TEST);
            }
            (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, old_read as u32);
            (self.functions.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, old_draw as u32);
        }
        unsafe {
            self.ready
                .signal(&self.functions, slot.image.texture, sync_value)
        };
        self.slots[slot_index]
            .release_value
            .store(sync_value, Ordering::Relaxed);
        self.scheduler.commit(slot_index, self.slots.len());
        self.sync_value = sync_value;
        let message = polymer_presentation::CapturedFrame {
            stream_id,
            slot: slot_index as u32,
            sync_value,
            timestamp_qpc: performance_counter().unwrap_or(0),
            extent: polymer_presentation::Extent2D {
                width: self.width,
                height: self.height,
            },
        }
        .present(polymer_presentation::FramePlacement::from_selection(
            source.map(|source| &source.selection),
            polymer_presentation::Extent2D {
                width: self.window_width,
                height: self.window_height,
            },
        ));
        ProducerSession::publish_frame(&mut self.ipc, message);
        Ok(Some(()))
    }
}

impl Drop for ExtCapture {
    fn drop(&mut self) {
        unsafe {
            (self.functions.delete_framebuffers)(1, &self.framebuffer);
            (self.functions.delete_framebuffers)(1, &self.read_framebuffer);
        }
    }
}

impl ExtCapture {
    #[expect(clippy::too_many_arguments)]
    fn publish(
        slots: &[ImageSlot],
        ready_handle: HANDLE,
        consumer_handle: HANDLE,
        adapter_luid: AdapterLuid,
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        bottom_up: bool,
        mut socket: ProducerSession,
    ) -> std::io::Result<ProducerSession> {
        let image_handles = slots
            .iter()
            .map(|slot| slot.image.handle.0 as isize as i64)
            .collect::<Vec<_>>();
        socket.publish(ExportDescriptor {
            surface_id,
            adapter_luid,
            source_window: source_window as i64,
            width,
            height,
            format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
            image_kind: ExternalImageKind::OpenGlD3d12Resource,
            sync_kind: ExternalSyncKind::D3d12Fence,
            image_handles: &image_handles,
            timeline_semaphore_handle: (ready_handle.0 as isize) as i64,
            consumer_semaphore_handle: (consumer_handle.0 as isize) as i64,
            bottom_up,
        })?;
        // The viewer acknowledges only accepted streams; a rejection closes the
        // socket instead. Without waiting for the ack, a rejected stream looks
        // alive forever (a stalled ring stops sending the frames that would
        // surface the dead socket) and reconnect retries never trigger.
        Ok(socket)
    }
}
