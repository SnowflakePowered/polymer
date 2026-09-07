use super::*;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_renderbuffer_names_do_not_inherit_inputs_or_invalidate_texture_names() {
        let tracking = Tracking::default();
        let first = tracking.context(1);
        let second = tracking.context(2);
        let mut buffer = crate::tracking::tests::candidate(7, 640, 480, 1);
        buffer.target = GL_RENDERBUFFER;
        buffer.incarnation = first.incarnation(true, 7);
        first.upsert_render_target(buffer);
        second.upsert_render_target(buffer);
        first.texture_info.insert(7, (GL_TEXTURE_2D, 640, 480));
        let texture = crate::tracking::tests::candidate(7, 640, 480, 1);
        let texture_incarnation = first.incarnation(false, 7);
        assert_ne!(buffer.incarnation, texture_incarnation);
        assert_ne!(texture.key(), buffer.key());
        first
            .observations
            .record(polymer_presentation::DependencyEdge::sampled(
                texture.key(),
                buffer.key(),
                0,
            ));
        first.forget_renderbuffer(7);
        assert!(first.resources.is_empty());
        assert_eq!(second.resources.len(), 1);
        assert!(
            first
                .observations
                .graph::<()>(0)
                .observation_list()
                .1
                .is_empty()
        );
        assert_eq!(
            *first.texture_info.get(&7).unwrap(),
            (GL_TEXTURE_2D, 640, 480)
        );
        // Reusing the numeric renderbuffer name cannot resurrect old edges.
        let old_incarnation = buffer.incarnation;
        buffer.incarnation = first.incarnation(true, 7);
        assert_ne!(buffer.incarnation, old_incarnation);
        assert_ne!(buffer.incarnation, texture_incarnation);
        assert_eq!(first.incarnation(false, 7), texture_incarnation);
        first.upsert_render_target(buffer);
        assert!(
            first
                .observations
                .graph::<()>(0)
                .observation_list()
                .1
                .is_empty()
        );
        first.forget_texture(7);
        assert_eq!(first.resources.len(), 1);
    }

    #[test]
    fn deleted_context_handle_cannot_reuse_old_texture_metadata() {
        let tracking = Tracking::default();
        let old_view = tracking.context(1);
        old_view.texture_info.insert(7, (0x0de1, 640, 480));
        tracking
            .context(2)
            .texture_info
            .insert(7, (0x0de1, 320, 240));

        // Successful WGL deletion can be reported without an available DC.
        // A reused context handle starts fresh even while an old view is held.
        tracking.context_deleted(1);
        let recreated = tracking.context(1);
        assert!(!recreated.texture_info.contains_key(&7));
        recreated.texture_info.insert(7, (0x0de1, 128, 96));
        assert_eq!(*old_view.texture_info.get(&7).unwrap(), (0x0de1, 640, 480));
        assert_eq!(*recreated.texture_info.get(&7).unwrap(), (0x0de1, 128, 96));
        assert_eq!(
            *tracking.context(2).texture_info.get(&7).unwrap(),
            (0x0de1, 320, 240)
        );
    }
}

/// GL object names are scoped to the context observing them. Selection and
/// metadata queries stay on the caller's current GL context.
#[derive(Default)]
pub struct Tracking {
    contexts: dashmap::DashMap<usize, Arc<ContextTracking>>,
}

impl Tracking {
    /// Invalidate metadata after WGL confirms context deletion. This makes no
    /// graphics calls and also works when no capture DC was ever recorded.
    pub fn context_deleted(&self, context: usize) {
        self.contexts.remove(&context);
    }

    /// # Safety
    /// `textures` references `count` texture names in the current GL context.
    pub unsafe fn textures_deleted(&self, count: i32, textures: *const Gluint) {
        if count <= 0 || textures.is_null() {
            return;
        }
        let context = self.context(unsafe { current_context() } as usize);
        for &texture in unsafe { std::slice::from_raw_parts(textures, count as usize) } {
            context.forget_texture(texture);
        }
    }

    /// # Safety
    /// `names` references `count` renderbuffer names in the current GL context.
    pub unsafe fn renderbuffers_deleted(&self, count: i32, names: *const Gluint) {
        if count <= 0 || names.is_null() {
            return;
        }
        let context = self.context(unsafe { current_context() } as usize);
        for &name in unsafe { std::slice::from_raw_parts(names, count as usize) } {
            context.forget_renderbuffer(name);
        }
    }
    fn context(&self, context: usize) -> Arc<ContextTracking> {
        self.contexts.entry(context).or_default().clone()
    }

    pub(crate) fn take_draw_candidates(
        &self,
        context: usize,
        backbuffer: Option<DrawCandidate>,
    ) -> (Vec<DrawCandidate>, bool) {
        self.context(context).take_draw_candidates(backbuffer)
    }

    pub(crate) fn refresh_sources(&self, sources: &[DrawCandidate]) -> Vec<DrawCandidate> {
        self.context(unsafe { current_context() } as usize)
            .refresh_sources(sources)
    }

    /// # Safety
    /// The application's rendering context must be current on this thread.
    pub unsafe fn record_draw_candidate(&self, range: Option<(i32, i32)>) {
        unsafe {
            self.context(current_context() as usize)
                .record_draw_candidate(range);
        }
    }

    /// # Safety
    /// The application's rendering context must be current on this thread.
    pub unsafe fn record_blit_candidate(&self, source: [i32; 4], destination: [i32; 4], mask: u32) {
        unsafe {
            self.context(current_context() as usize)
                .record_blit_candidate(source, destination, mask);
        }
    }

    /// # Safety
    /// The application's rendering context and copy resources must be live.
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
        unsafe {
            self.context(current_context() as usize)
                .record_copy_candidate(
                    src_name,
                    src_target,
                    source,
                    dst_name,
                    dst_target,
                    destination,
                    extent,
                );
        }
    }
}

impl ContextTracking {
    fn forget_resource_observations(&self, name: Gluint, renderbuffer: bool) {
        self.observations
            .forget_matching(|key| key.renderbuffer == renderbuffer && key.texture == name);
    }

    fn forget_renderbuffer(&self, name: Gluint) {
        self.incarnations.remove(&(true, name));
        self.resources.retain(|_, candidate| {
            candidate.target != GL_RENDERBUFFER || candidate.texture != name
        });
        self.forget_resource_observations(name, true);
    }

    pub(super) fn forget_texture(&self, texture: Gluint) {
        self.incarnations.remove(&(false, texture));
        self.texture_info.remove(&texture);
        self.resources.retain(|_, candidate| {
            candidate.target == GL_RENDERBUFFER || candidate.texture != texture
        });
        self.forget_resource_observations(texture, false);
    }
}
