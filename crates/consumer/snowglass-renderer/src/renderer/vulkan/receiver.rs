//! Import side of the Vulkan renderer.

use ash::vk;
use snowglass_consumer_vulkan::{ImportError, ImportedImageRing};
use snowglass_ipc::StreamReady;

/// Vulkan device context dedicated to importing producer stream handles.
///
/// The instance and device are shallow Vulkan handle clones. Their owner must
/// keep the underlying objects alive until the receiver and every returned
/// [`ImportedImageRing`] have been dropped.
pub struct VulkanReceiver {
    instance: ash::Instance,
    device: ash::Device,
    physical_device: vk::PhysicalDevice,
}

impl VulkanReceiver {
    pub fn new(
        instance: ash::Instance,
        device: ash::Device,
        physical_device: vk::PhysicalDevice,
    ) -> Self {
        Self {
            instance,
            device,
            physical_device,
        }
    }

    pub fn import_stream(&self, stream: &StreamReady) -> Result<ImportedImageRing, ImportError> {
        ImportedImageRing::import_into(
            &self.instance,
            self.device.clone(),
            self.physical_device,
            stream,
        )
    }
}
