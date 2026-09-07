use super::*;

impl ContextTracking {
    pub(super) unsafe fn measure_renderbuffer(&self, name: Gluint) -> Option<(u32, u32, u32)> {
        type IsRenderbuffer = unsafe extern "system" fn(u32) -> u8;
        type Query = unsafe extern "system" fn(u32, u32, *mut i32);
        let exists: IsRenderbuffer = unsafe { proc(b"glIsRenderbuffer\0") }?;
        if unsafe { exists(name) } == 0 {
            return None;
        }
        let mut values = [0; 3];
        let parameters = [0x8D42, 0x8D43, 0x8CAB]; // width, height, samples
        if let Some(query) = unsafe { proc::<Query>(b"glGetNamedRenderbufferParameteriv\0") } {
            for (value, parameter) in values.iter_mut().zip(parameters) {
                unsafe { query(name, parameter, value) };
            }
        } else {
            let bind: crate::interop::GlBindFramebuffer = unsafe { proc(b"glBindRenderbuffer\0") }?;
            let query: Query = unsafe { proc(b"glGetRenderbufferParameteriv\0") }?;
            let mut previous = 0;
            unsafe {
                glGetIntegerv(0x8CA7, &mut previous);
                bind(GL_RENDERBUFFER, name);
                for (value, parameter) in values.iter_mut().zip(parameters) {
                    query(GL_RENDERBUFFER, parameter, value);
                }
                bind(GL_RENDERBUFFER, previous as u32);
            }
        }
        (values[0] > 0 && values[1] > 0 && values[2] >= 0).then_some((
            values[0] as u32,
            values[1] as u32,
            values[2] as u32,
        ))
    }

    pub(super) unsafe fn renderbuffer_candidate(&self, name: Gluint) -> Option<DrawCandidate> {
        let (width, height, _) = unsafe { self.measure_renderbuffer(name) }?;
        let incarnation = self.incarnation(true, name);
        Some(DrawCandidate {
            selection: Default::default(),
            texture: name,
            target: GL_RENDERBUFFER,
            incarnation,
            mip: 0,
            layer: 0,
            width,
            height,
        })
    }

    pub(super) fn refresh_renderbuffer(&self, source: &DrawCandidate) -> bool {
        source.valid_crop()
            && self
                .incarnations
                .get(&(true, source.texture))
                .is_some_and(|id| *id == source.incarnation)
            && unsafe { self.measure_renderbuffer(source.texture) }.is_some_and(
                |(width, height, samples)| {
                    samples <= 1 && (width, height) == (source.width, source.height)
                },
            )
    }
}
