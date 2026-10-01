use super::*;

impl ExtCapture {
    pub(super) fn connected(&self) -> bool {
        self.ipc.as_ref().is_some_and(ProducerSession::connected)
    }
    pub(super) unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        connection: ProducerSession,
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
        unsafe {
            (functions.gen_framebuffers)(1, &mut framebuffer);
        }
        let mut capture = Self {
            width,
            height,
            functions,
            framebuffer,
            slots,
            _device: device,
            ready,
            consumer,
            ipc: None,
            scheduler: Default::default(),
            sync_value: 0,
        };
        let mut previous_draw = 0;
        unsafe {
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut previous_draw);
            (functions.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, framebuffer);
        }
        let complete = capture.slots.iter().all(|slot| unsafe {
            (functions.framebuffer_texture_2d)(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                slot.image.texture,
                0,
            );
            (functions.check_framebuffer)(GL_DRAW_FRAMEBUFFER) == 0x8CD5 // GL_FRAMEBUFFER_COMPLETE
        });
        unsafe { (functions.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, previous_draw as u32) };
        if !complete {
            return Err("OpenGL EXT shared images are not framebuffer-complete".into());
        }
        let publication = capture.publish(surface_id, source_window, adapter_luid, connection);
        capture.ipc =
            ProducerSession::setup_result(publication).map_err(|error| error.to_string())?;
        tracing::debug!(
            "polymer: attached OpenGL EXT ring {width}x{height} (surface {:#x}, window {source_window:#x})",
            surface_id.0
        );
        Ok(capture)
    }

    pub(super) unsafe fn copy_frame(&mut self, stream_id: StreamId) {
        let completed = unsafe { self.consumer.completed_value() };
        let sync_value = self.sync_value.wrapping_add(1).max(1);
        let Some(reservation) = self.scheduler.try_acquire(self.slots.len(), |index| {
            (self.slots[index].release_value.load(Ordering::Relaxed) <= completed).then_some(())
        }) else {
            return;
        };
        let slot_index = reservation.0;
        let slot = &self.slots[slot_index];
        unsafe {
            self.consumer.wait(
                slot.image.texture,
                slot.release_value.load(Ordering::Relaxed),
            );
        }
        let mut old_read = 0;
        let mut old_draw = 0;
        let mut old_buffer = 0;
        let mut double_buffered = 0;
        let scissor_enabled = unsafe { glIsEnabled(GL_SCISSOR_TEST) } != 0;
        let srgb_enabled = unsafe { glIsEnabled(crate::interop::GL_FRAMEBUFFER_SRGB) } != 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut old_read);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut old_draw);
            (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, 0);
            (self.functions.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
            glGetIntegerv(crate::interop::GL_READ_BUFFER, &mut old_buffer);
            glGetIntegerv(crate::interop::GL_DOUBLEBUFFER, &mut double_buffered);
            crate::interop::glReadBuffer(if double_buffered != 0 {
                crate::interop::GL_BACK
            } else {
                crate::interop::GL_FRONT
            });
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
            if srgb_enabled {
                glDisable(crate::interop::GL_FRAMEBUFFER_SRGB);
            }

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
            crate::interop::glReadBuffer(old_buffer as u32);
            if scissor_enabled {
                glEnable(GL_SCISSOR_TEST);
            }
            if srgb_enabled {
                glEnable(crate::interop::GL_FRAMEBUFFER_SRGB);
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
        let message = polymer_protocol::FrameReady {
            stream_id,
            slot: slot_index as u32,
            frame_number: 0,
            sync_value,
            timestamp_qpc: performance_counter().unwrap_or(0),
        };
        ProducerSession::publish_frame(&mut self.ipc, message);
    }
}

impl Drop for ExtCapture {
    fn drop(&mut self) {
        unsafe {
            (self.functions.delete_framebuffers)(1, &self.framebuffer);
        }
    }
}

impl ExtCapture {
    fn publish(
        &self,
        surface_id: SurfaceId,
        source_window: isize,
        adapter_luid: AdapterLuid,
        mut socket: ProducerSession,
    ) -> std::io::Result<ProducerSession> {
        let image_handles = self
            .slots
            .iter()
            .map(|slot| slot.image.handle.0 as isize as i64)
            .collect::<Vec<_>>();
        socket.publish(ExportDescriptor {
            surface_id,
            adapter_luid,
            source_window: source_window as i64,
            hook: GraphicsHook::OpenGl,
            width: self.width,
            height: self.height,
            format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
            image_kind: ExternalImageKind::D3d12Resource,
            sync_kind: ExternalSyncKind::D3d12Fence,
            image_handles: &image_handles,
            timeline_semaphore_handle: self.ready.handle().0 as i64,
            consumer_semaphore_handle: self.consumer.handle().0 as i64,
            bottom_up: true,
        })?;
        // The viewer acknowledges only accepted streams; a rejection closes the
        // socket instead. Without waiting for the ack, a rejected stream looks
        // alive forever (a stalled ring stops sending the frames that would
        // surface the dead socket) and reconnect retries never trigger.
        Ok(socket)
    }
}
