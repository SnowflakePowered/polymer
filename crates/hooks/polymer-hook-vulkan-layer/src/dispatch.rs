//! Loader dispatch bookkeeping for native devices and surfaces.
use crate::layer::{
    PhysicalDeviceState, QueueDispatchTable, VulkanHookState, dispatch_key, instance_gipa,
};
use ash::vk;

impl VulkanHookState {
    pub(crate) fn queue_created(&self, device: vk::Device, queue: *mut vk::Queue) {
        let Some(queue) = (unsafe { queue.as_ref() })
            .copied()
            .filter(|queue| *queue != vk::Queue::null())
        else {
            return;
        };
        let Some(dispatch) = (unsafe { dispatch_key(device) })
            .and_then(|key| self.device.get(&key).map(|entry| entry.clone()))
        else {
            return;
        };
        self.queue
            .insert(queue, QueueDispatchTable { device, dispatch });
    }

    pub(crate) fn physical_devices_enumerated(
        &self,
        instance: vk::Instance,
        count: *mut u32,
        devices: *mut vk::PhysicalDevice,
        result: vk::Result,
    ) {
        if !matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
            || count.is_null()
            || devices.is_null()
        {
            return;
        }
        let Some(next_gipa) = instance_gipa(instance) else {
            return;
        };
        for &device in unsafe { std::slice::from_raw_parts(devices, *count as usize) } {
            self.physical_device.insert(
                device,
                PhysicalDeviceState {
                    instance,
                    next_gipa,
                },
            );
        }
    }
}
