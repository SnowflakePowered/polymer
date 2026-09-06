use ash::vk;
use dashmap::DashMap;
use std::sync::Arc;

/// Stable binary semaphores indexed by logical stream and swapchain image.
/// The swapchain owns this pool independently of capture stream lifetimes.
#[derive(Default)]
pub(crate) struct PresentationSemaphores {
    semaphores: DashMap<(u64, u32), Arc<PresentationSemaphore>>,
}
pub(crate) struct PresentationSemaphore {
    device: ash::Device,
    semaphore: vk::Semaphore,
}
impl PresentationSemaphores {
    pub(super) fn stream(
        &self,
        device: &ash::Device,
        stream: u64,
        images: usize,
    ) -> Result<Vec<Arc<PresentationSemaphore>>, vk::Result> {
        (0..images)
            .map(|image| {
                let key = (stream, image as u32);
                match self.semaphores.entry(key) {
                    dashmap::mapref::entry::Entry::Occupied(entry) => Ok(entry.get().clone()),
                    dashmap::mapref::entry::Entry::Vacant(entry) => {
                        let semaphore = unsafe {
                            device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                        }?;
                        let owned = Arc::new(PresentationSemaphore {
                            device: device.clone(),
                            semaphore,
                        });
                        entry.insert(owned.clone());
                        Ok(owned)
                    }
                }
            })
            .collect()
    }
}
impl PresentationSemaphore {
    pub(super) fn raw(&self) -> vk::Semaphore {
        self.semaphore
    }
}
impl Drop for PresentationSemaphore {
    fn drop(&mut self) {
        // Final pool retirement follows swapchain/device destruction, whose
        // native synchronization remains the hook caller's responsibility.
        unsafe { self.device.destroy_semaphore(self.semaphore, None) };
    }
}
