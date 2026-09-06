use ash::vk;
use dashmap::DashMap;
use std::sync::Arc;

/// Stable binary semaphores indexed by logical stream and swapchain image.
/// The swapchain owns this pool independently of capture stream lifetimes.
#[derive(Default)]
pub(crate) struct PresentationSemaphores {
    semaphores: DashMap<(u64, u32), Arc<PresentationSemaphore>>,
    completions: parking_lot::Mutex<Vec<Arc<crate::capture::PresentationFence>>>,
}
pub(crate) struct PresentationSemaphore {
    device: ash::Device,
    semaphore: vk::Semaphore,
    retained_without_completion: std::sync::atomic::AtomicBool,
}
impl PresentationSemaphores {
    pub(crate) fn retain_signals(
        &self,
        signals: &[vk::Semaphore],
    ) -> Vec<Arc<PresentationSemaphore>> {
        self.semaphores
            .iter()
            .filter(|entry| signals.contains(&entry.semaphore))
            .map(|entry| entry.value().clone())
            .collect()
    }
    pub(crate) fn record_completion(&self, completion: Arc<crate::capture::PresentationFence>) {
        let mut pending = self.completions.lock();
        pending.retain(|fence| !fence.complete());
        pending.push(completion);
    }
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
                            retained_without_completion: Default::default(),
                        });
                        entry.insert(owned.clone());
                        Ok(owned)
                    }
                }
            })
            .collect()
    }
}
impl Drop for PresentationSemaphores {
    fn drop(&mut self) {
        if self
            .completions
            .get_mut()
            .iter()
            .any(|completion| !completion.wait())
        {
            // An unexpected native wait failure is not permission to destroy
            // semaphore payloads still referenced by the presentation engine.
            std::mem::forget(std::mem::take(&mut self.semaphores));
            std::mem::forget(std::mem::take(self.completions.get_mut()));
        }
    }
}
impl PresentationSemaphore {
    /// Retain one reference when presentation completion cannot be proven.
    /// Repeated presents of the same image must not leak another reference.
    pub(crate) fn retain_without_completion(self: &Arc<Self>) {
        if !self
            .retained_without_completion
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            std::mem::forget(self.clone());
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_stream_semaphores_remain_owned_and_reusable_by_the_swapchain() {
        unsafe {
            let entry =
                ash::Entry::load().expect("Vulkan loader required for native semaphore fixture");
            let instance = entry
                .create_instance(&vk::InstanceCreateInfo::default(), None)
                .unwrap();
            let physical = instance.enumerate_physical_devices().unwrap()[0];
            let family = instance
                .get_physical_device_queue_family_properties(physical)
                .iter()
                .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .unwrap() as u32;
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&priorities)];
            let device = instance
                .create_device(
                    physical,
                    &vk::DeviceCreateInfo::default().queue_create_infos(&queues),
                    None,
                )
                .unwrap();
            let queue = device.get_device_queue(family, 0);
            let pool = PresentationSemaphores::default();
            let stream = pool.stream(&device, 10, 2).unwrap();
            let first = stream[0].raw();
            let second_image = stream[1].raw();
            let sibling = pool.stream(&device, 20, 2).unwrap();
            assert_ne!(first, second_image);
            assert_ne!(first, sibling[0].raw());
            let signals = [first];
            device
                .queue_submit(
                    queue,
                    &[vk::SubmitInfo::default().signal_semaphores(&signals)],
                    vk::Fence::null(),
                )
                .unwrap();
            // Stream retirement drops its references while the queued signal
            // still owns the semaphore. The replacement must retrieve it alive.
            drop(stream);
            let replacement = pool.stream(&device, 10, 2).unwrap();
            assert_eq!(replacement[0].raw(), first);
            assert_eq!(replacement[1].raw(), second_image);
            let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
            let sibling_signal = [sibling[0].raw()];
            device
                .queue_submit(
                    queue,
                    &[vk::SubmitInfo::default()
                        .wait_semaphores(&signals)
                        .wait_dst_stage_mask(&stages)
                        .signal_semaphores(&sibling_signal)],
                    vk::Fence::null(),
                )
                .unwrap();
            let fence = device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .unwrap();
            // A pending retry keeps its signal payloads even if the original
            // streams and pool disappear before the replacement wait is queued.
            let retained = pool.retain_signals(&[first, sibling_signal[0]]);
            assert_eq!(retained.len(), 2);
            let references = Arc::strong_count(&retained[0]);
            for _ in 0..10 {
                retained[0].retain_without_completion();
            }
            assert_eq!(Arc::strong_count(&retained[0]), references + 1);
            drop(replacement);
            drop(sibling);
            drop(pool);
            device
                .queue_submit(
                    queue,
                    &[vk::SubmitInfo::default()
                        .wait_semaphores(&sibling_signal)
                        .wait_dst_stage_mask(&stages)],
                    fence,
                )
                .unwrap();
            device
                .wait_for_fences(&[fence], true, 2_000_000_000)
                .unwrap();
            device.destroy_fence(fence, None);
            // This fixture has an explicit completed wait, so it can reclaim
            // the single extra reference retained above without leaking it.
            drop(Arc::from_raw(Arc::as_ptr(&retained[0])));
            drop(retained);
            device.destroy_device(None);
            instance.destroy_instance(None);
        }
    }
}
