use super::*;
use crate::layer::{CaptureDevice, PhysicalDeviceState};
use parking_lot::Mutex;
use polymer_producer_vulkan::virtual_swapchain::native::Queue;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) struct Device {
    pub capture: Arc<CaptureDevice>,
    pub physical: vk::PhysicalDevice,
    pub instance: PhysicalDeviceState,
    pub queues: BTreeMap<u64, Arc<Queue>>,
    pub acquire_queue: Arc<Queue>,
    pub families: Vec<u32>,
    pub alive: Arc<AtomicBool>,
    pub lost: AtomicBool,
    pub images: DashMap<vk::Image, ()>,
    pub layouts: layouts::Layouts,
    pub retired: Mutex<Vec<BackendRetirement<VulkanVirtualBackend>>>,
    pub(super) native_presents: Mutex<Vec<present::NativePresent>>,
}
impl Device {
    pub(crate) unsafe fn install(
        handle: vk::Device,
        info: &vk::DeviceCreateInfo<'_>,
    ) -> Result<(), vk::Result> {
        if !unsafe { features::Features::virtual_device(info) } {
            return Ok(());
        }
        let dispatch = unsafe { crate::layer::dispatch_key(handle) }
            .and_then(|key| {
                crate::layer::hook_state()
                    .device
                    .get(&key)
                    .map(|d| d.clone())
            })
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let capture = dispatch
            .capture
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        if !capture.supports_foreign_queue {
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        }
        let instance = dispatch
            .physical_state
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let properties = Surface::queue_properties(dispatch.physical_device, &instance)?;
        let alive = Arc::new(AtomicBool::new(true));
        let mut queues = BTreeMap::new();
        let mut acquire_queue = None;
        let mut families = Vec::new();
        for info in unsafe {
            std::slice::from_raw_parts(
                info.p_queue_create_infos,
                info.queue_create_info_count as usize,
            )
        } {
            if !info.flags.is_empty() {
                continue;
            }
            families.push(info.queue_family_index);
            for index in 0..info.queue_count {
                let queue = unsafe {
                    capture
                        .device
                        .get_device_queue(info.queue_family_index, index)
                };
                let owner = Queue::new(
                    capture.clone(),
                    queue,
                    info.queue_family_index,
                    alive.clone(),
                );
                if acquire_queue.is_none()
                    && properties[info.queue_family_index as usize]
                        .queue_flags
                        .contains(vk::QueueFlags::GRAPHICS)
                {
                    acquire_queue = Some(owner.clone());
                }
                // Register even queues the application has not requested yet.
                crate::layer::hook_state().queue.insert(
                    queue,
                    crate::layer::QueueDispatchTable {
                        device: handle,
                        queue_family_index: info.queue_family_index,
                        dispatch: crate::layer::hook_state()
                            .device
                            .get(&unsafe { crate::layer::dispatch_key(handle) }.unwrap())
                            .unwrap()
                            .clone(),
                    },
                );
                queues.insert(queue.as_raw(), owner);
            }
        }
        families.sort_unstable();
        families.dedup();
        let Some(acquire_queue) = acquire_queue else {
            return Ok(());
        };
        DEVICES.insert(
            handle,
            Arc::new(Self {
                capture,
                physical: dispatch.physical_device,
                instance,
                queues,
                acquire_queue,
                families,
                alive,
                lost: AtomicBool::new(false),
                images: DashMap::new(),
                layouts: Default::default(),
                retired: Mutex::new(Vec::new()),
                native_presents: Mutex::new(Vec::new()),
            }),
        );
        Ok(())
    }
    pub(super) fn poll_retired(&self) {
        let mut presents = std::mem::take(&mut *self.native_presents.lock());
        presents.retain(|present| !present.complete());
        self.native_presents.lock().extend(presents);
        let mut work = std::mem::take(&mut *self.retired.lock());
        work.retain_mut(|item| item.poll() != RetirementProgress::Safe);
        self.retired.lock().extend(work);
    }
    pub(crate) fn retains_instance(instance: vk::Instance) -> bool {
        DEVICES
            .iter()
            .any(|device| device.instance.instance == instance)
    }
    pub(crate) fn destroy(handle: vk::Device) -> bool {
        NATIVES.retain(|_, native| native.device != handle);
        let Ok(device) = Self::get(handle) else {
            return true;
        };
        for queue in device.queues.values() {
            if queue.drain().is_err() {
                device.lost.store(true, Ordering::Release);
                tracing::error!(
                    "virtual Vulkan device retained: queue completion is indeterminate"
                );
                return false;
            }
        }
        let handles = SWAPCHAINS
            .iter()
            .filter(|s| s.device.capture.device.handle() == handle)
            .map(|s| *s.key())
            .collect::<Vec<_>>();
        for handle in handles {
            if let Some((_, swapchain)) = SWAPCHAINS.remove(&handle) {
                swapchain.retire();
            }
        }
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            device.poll_retired();
            if device.retired.lock().is_empty() && device.native_presents.lock().is_empty() {
                break;
            }
            if Instant::now() >= until {
                tracing::error!("virtual Vulkan device retained: external ownership has not ended");
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        device.layouts.destroy(&device.capture.device);
        device.alive.store(false, Ordering::Release);
        DEVICES.remove(&handle);
        true
    }
}
