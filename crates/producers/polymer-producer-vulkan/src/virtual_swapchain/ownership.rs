use super::*;

impl VulkanVirtualBackend {
    fn local_family(&self, family: u32) -> u32 {
        if self.concurrent {
            vk::QUEUE_FAMILY_IGNORED
        } else {
            family
        }
    }
    pub(super) fn acquire_barrier(&self, slot: ImageSlot) -> vk::ImageMemoryBarrier<'static> {
        let initialized = self.initialized[slot.index() as usize];
        vk::ImageMemoryBarrier::default()
            .image(self.api_access(slot))
            .old_layout(if initialized {
                vk::ImageLayout::GENERAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(if initialized {
                vk::QUEUE_FAMILY_FOREIGN_EXT
            } else {
                vk::QUEUE_FAMILY_IGNORED
            })
            .dst_queue_family_index(if initialized {
                self.local_family(self.acquire_queue.family)
            } else {
                vk::QUEUE_FAMILY_IGNORED
            })
            .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            )
    }
    pub(super) fn release_barrier(
        &self,
        slot: ImageSlot,
        family: u32,
    ) -> vk::ImageMemoryBarrier<'static> {
        vk::ImageMemoryBarrier::default()
            .image(self.api_access(slot))
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(self.local_family(family))
            .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE | vk::AccessFlags::MEMORY_READ)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            )
    }
    pub(super) fn retirement(&self, work: &RetirementWork<Self>) -> RetirementProgress {
        if !work.uncertain.is_empty() {
            return RetirementProgress::Quarantined;
        }
        for submission in work
            .submissions
            .iter()
            .map(|v| &v.1)
            .chain(work.acquired.iter().map(|v| &v.1.acquire))
        {
            match submission.complete() {
                Ok(true) => {}
                Ok(false) => return RetirementProgress::Pending,
                Err(_) => return RetirementProgress::Quarantined,
            }
        }
        match unsafe {
            self.capture
                .device
                .get_semaphore_counter_value(self.released.semaphore())
        } {
            Ok(value) if work.publications.iter().any(|(_, needed)| value < *needed) => {
                RetirementProgress::Pending
            }
            Ok(_) => RetirementProgress::Safe,
            Err(_) => RetirementProgress::Quarantined,
        }
    }
}
