use super::*;
use polymer_presentation::swapchain::FixedSurfaceBinding;

#[derive(Default)]
pub(super) struct CaptureStore {
    pub(super) entries: dashmap::DashMap<(usize, usize), Mutex<Capture>>,
}
impl CaptureStore {
    pub(super) unsafe fn present_fixed(
        &self,
        hdc: usize,
        context: usize,
        binding: &FixedSurfaceBinding,
    ) {
        let extent = binding.extent();
        let surface = SurfaceId(binding.logical_id());
        let stored = self.entries.entry((hdc, context)).or_insert_with(|| {
            Mutex::new(Capture::new(
                surface,
                binding.window().handle as isize,
                extent.width,
                extent.height,
            ))
        });
        let mut capture = stored.lock();
        unsafe {
            capture.tick();
            capture.copy_frame();
        }
    }
    pub(super) fn context_destroyed(&self, context: usize) {
        let keys = self
            .entries
            .iter()
            .filter_map(|entry| (entry.key().1 == context).then_some(*entry.key()))
            .collect::<Vec<_>>();
        for key in keys {
            if let Some((_, capture)) = self.entries.remove(&key) {
                capture.into_inner().context_destroyed();
            }
        }
    }
    pub(super) unsafe fn retire_context(&self, context: usize) {
        self.entries.retain(|&(_, owner), _| owner != context);
    }
}
