use crate::interop::*;

pub(super) struct Renderbuffer {
    pub name: u32,
    delete: unsafe extern "system" fn(i32, *const u32),
}

impl Renderbuffer {
    pub unsafe fn selected_source(&self) -> crate::tracking::DrawCandidate {
        unsafe {
            let gen_fbo: GlGenFramebuffers = proc(b"glGenFramebuffers\0").unwrap();
            let bind: GlBindFramebuffer = proc(b"glBindFramebuffer\0").unwrap();
            let delete: GlDeleteFramebuffers = proc(b"glDeleteFramebuffers\0").unwrap();
            let attach: GlFramebufferRenderbuffer = proc(b"glFramebufferRenderbuffer\0").unwrap();
            let blit: GlBlitFramebuffer = proc(b"glBlitFramebuffer\0").unwrap();
            let mut previous = [0; 2];
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut previous[0]);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut previous[1]);
            let mut fbo = 0;
            gen_fbo(1, &mut fbo);
            bind(GL_READ_FRAMEBUFFER, fbo);
            attach(
                GL_READ_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0 + 1,
                GL_RENDERBUFFER,
                self.name,
            );
            windows_sys::Win32::Graphics::OpenGL::glReadBuffer(GL_COLOR_ATTACHMENT0 + 1);
            bind(GL_DRAW_FRAMEBUFFER, 0);
            blit(4, 8, 20, 20, 2, 3, 18, 15, GL_COLOR_BUFFER_BIT, GL_NEAREST);
            let variables = [
                (polymer_hook::config::TRACK_RENDER_TARGET, "1"),
                (polymer_hook::config::TRACK_RENDER_TARGETS, "0"),
                (polymer_hook::config::GRAPH_TRACKING, "1"),
            ];
            let saved = variables.map(|(name, _)| std::env::var_os(name));
            for (name, value) in variables {
                std::env::set_var(name, value);
            }
            let tracking = crate::tracking::Tracking::default();
            let mut draw_buffer = 0;
            glGetIntegerv(0x0C01, &mut draw_buffer);
            windows_sys::Win32::Graphics::OpenGL::glDrawBuffer(0);
            blit(4, 8, 20, 20, 2, 3, 18, 15, GL_COLOR_BUFFER_BIT, GL_NEAREST);
            tracking.record_blit_candidate([4, 8, 20, 20], [2, 3, 18, 15], GL_COLOR_BUFFER_BIT);
            assert!(
                tracking
                    .take_draw_candidates(current_context() as usize, None)
                    .0
                    .is_empty()
            );
            windows_sys::Win32::Graphics::OpenGL::glDrawBuffer(draw_buffer as u32);
            blit(4, 8, 20, 20, 2, 3, 18, 15, GL_COLOR_BUFFER_BIT, GL_NEAREST);
            tracking.record_blit_candidate([4, 8, 20, 20], [2, 3, 18, 15], GL_COLOR_BUFFER_BIT);
            let (selected, exact) = tracking.take_draw_candidates(current_context() as usize, None);
            for ((name, _), value) in variables.into_iter().zip(saved) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            bind(GL_READ_FRAMEBUFFER, previous[0] as u32);
            bind(GL_DRAW_FRAMEBUFFER, previous[1] as u32);
            delete(1, &fbo);
            assert!(exact);
            assert_eq!(selected.len(), 1);
            assert_eq!(
                (selected[0].target, selected[0].texture),
                (GL_RENDERBUFFER, self.name)
            );
            assert_eq!(selected[0].source_origin, (4, 8));
            assert_eq!((selected[0].width, selected[0].height), (16, 12));
            assert_eq!(tracking.refresh_sources(&selected).len(), 1);
            selected[0]
        }
    }

    pub unsafe fn from_texture(texture: u32) -> Self {
        unsafe {
            let generate: GlGenFramebuffers = proc(b"glGenRenderbuffers\0").unwrap();
            let delete = proc(b"glDeleteRenderbuffers\0").unwrap();
            let bind: GlBindFramebuffer = proc(b"glBindRenderbuffer\0").unwrap();
            let storage: unsafe extern "system" fn(u32, u32, i32, i32) =
                proc(b"glRenderbufferStorage\0").unwrap();
            let gen_fbo: GlGenFramebuffers = proc(b"glGenFramebuffers\0").unwrap();
            let bind_fbo: GlBindFramebuffer = proc(b"glBindFramebuffer\0").unwrap();
            let delete_fbo: GlDeleteFramebuffers = proc(b"glDeleteFramebuffers\0").unwrap();
            let attach_texture: GlFramebufferTexture2D = proc(b"glFramebufferTexture2D\0").unwrap();
            let attach: GlFramebufferRenderbuffer = proc(b"glFramebufferRenderbuffer\0").unwrap();
            let blit: GlBlitFramebuffer = proc(b"glBlitFramebuffer\0").unwrap();
            let mut previous = [0; 3];
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut previous[0]);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut previous[1]);
            glGetIntegerv(0x8CA7, &mut previous[2]);
            let mut name = 0;
            generate(1, &mut name);
            bind(GL_RENDERBUFFER, name);
            storage(GL_RENDERBUFFER, 0x8058, 64, 64);
            let mut fbos = [0; 2];
            gen_fbo(2, fbos.as_mut_ptr());
            bind_fbo(GL_READ_FRAMEBUFFER, fbos[0]);
            attach_texture(
                GL_READ_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                texture,
                0,
            );
            bind_fbo(GL_DRAW_FRAMEBUFFER, fbos[1]);
            attach(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_RENDERBUFFER,
                name,
            );
            blit(0, 0, 64, 64, 0, 0, 64, 64, GL_COLOR_BUFFER_BIT, GL_NEAREST);
            bind_fbo(GL_READ_FRAMEBUFFER, previous[0] as u32);
            bind_fbo(GL_DRAW_FRAMEBUFFER, previous[1] as u32);
            bind(GL_RENDERBUFFER, previous[2] as u32);
            delete_fbo(2, fbos.as_ptr());
            Self { name, delete }
        }
    }
}

impl Drop for Renderbuffer {
    fn drop(&mut self) {
        unsafe { (self.delete)(1, &self.name) };
    }
}
