pub(crate) struct VulkanFrame {
    pub(crate) instance: ash::Instance,
    pub(crate) physical_device: ash::vk::PhysicalDevice,
    pub(crate) device: ash::Device,
    pub(crate) queue: ash::vk::Queue,
    pub(crate) queue_family: u32,
    pub(crate) conversion_queue: ash::vk::Queue,
    pub(crate) conversion_queue_family: u32,
    pub(crate) image: ash::vk::Image,
    pub(crate) format: ash::vk::Format,
    pub(crate) layout: ash::vk::ImageLayout,
    pub(crate) ready: ash::vk::Semaphore,
    pub(crate) ready_value: u64,
    pub(crate) completion: ash::vk::Semaphore,
    pub(crate) completion_value: u64,
    pub(crate) completion_submitted: bool,
}

impl VulkanFrame {
    pub(crate) fn mark_completion_submitted(&mut self) {
        self.completion_submitted = true;
    }

    pub(crate) fn completion_submitted(&self) -> bool {
        self.completion_submitted
    }
}

impl Drop for VulkanFrame {
    fn drop(&mut self) {
        if !self.completion_submitted {
            let _ = unsafe {
                self.device.signal_semaphore(
                    &ash::vk::SemaphoreSignalInfo::default()
                        .semaphore(self.completion)
                        .value(self.completion_value),
                )
            };
        }
    }
}
