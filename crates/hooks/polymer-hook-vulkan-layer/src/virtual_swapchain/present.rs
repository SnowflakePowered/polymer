use super::*;
use polymer_producer_vulkan::virtual_swapchain::{PresentInfo, native::Semaphore};
use std::{sync::atomic::Ordering, time::Instant};

pub(super) struct NativePresent {
    capture: Arc<crate::layer::CaptureDevice>,
    fences: Vec<vk::Fence>,
    bridge: Option<Arc<Semaphore>>,
    submitted: bool,
}
impl NativePresent {
    fn new(device: &Device, count: usize, bridge: Arc<Semaphore>) -> Result<Self, vk::Result> {
        let mut work = Self {
            capture: device.capture.clone(),
            fences: Vec::new(),
            bridge: Some(bridge),
            submitted: false,
        };
        for _ in 0..count {
            work.fences.push(unsafe {
                device
                    .capture
                    .device
                    .create_fence(&Default::default(), None)?
            });
        }
        Ok(work)
    }
    pub(super) fn complete(&self) -> bool {
        !self.submitted
            || self
                .fences
                .iter()
                .all(|&fence| unsafe { self.capture.device.get_fence_status(fence) } == Ok(true))
    }
}
impl Drop for NativePresent {
    fn drop(&mut self) {
        if !self.complete() {
            std::mem::forget(self.bridge.take());
            return;
        }
        for &fence in &self.fences {
            unsafe {
                self.capture.device.destroy_fence(fence, None);
            }
        }
    }
}

struct Entry {
    ordinal: usize,
    index: u32,
    swapchain: Arc<wsi::Swapchain>,
}
struct PresentBatch;
impl PresentBatch {
    unsafe fn run(
        queue_handle: vk::Queue,
        info: &vk::PresentInfoKHR<'_>,
    ) -> Result<vk::Result, vk::Result> {
        let Some(device) = Device::queue(queue_handle) else {
            let (gdpa, handle) = crate::layer::queue_dispatch_data(queue_handle)
                .ok_or(vk::Result::ERROR_DEVICE_LOST)?;
            let next: vk::PFN_vkQueuePresentKHR =
                unsafe { crate::terminal::device_proc(gdpa, handle, c"vkQueuePresentKHR") }
                    .ok_or(vk::Result::ERROR_DEVICE_LOST)?;
            return Ok(unsafe { next(queue_handle, info) });
        };
        if device.lost.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        let queue = device.queues.get(&queue_handle.as_raw()).cloned();
        let handles =
            unsafe { std::slice::from_raw_parts(info.p_swapchains, info.swapchain_count as usize) };
        let indices = unsafe {
            std::slice::from_raw_parts(info.p_image_indices, info.swapchain_count as usize)
        };
        let mut entries = Vec::new();
        let mut native_ordinals = Vec::new();
        for (ordinal, (&handle, &index)) in handles.iter().zip(indices).enumerate() {
            if let Some(swapchain) = wsi::Swapchain::get(device.capture.device.handle(), handle)? {
                entries.push(Entry {
                    ordinal,
                    index,
                    swapchain,
                });
            } else {
                native_ordinals.push(ordinal);
            }
        }
        let next: vk::PFN_vkQueuePresentKHR =
            crate::layer::device_gdpa(device.capture.device.handle())
                .and_then(|g| unsafe {
                    crate::terminal::device_proc(
                        g,
                        device.capture.device.handle(),
                        c"vkQueuePresentKHR",
                    )
                })
                .ok_or(vk::Result::ERROR_DEVICE_LOST)?;
        if entries.is_empty() {
            let _guard = queue.as_ref().map(|queue| queue.gate.lock());
            return Ok(unsafe { next(queue_handle, info) });
        }
        let queue = queue.ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let native_handles = native_ordinals
            .iter()
            .map(|&i| handles[i])
            .collect::<Vec<_>>();
        let native_indices = native_ordinals
            .iter()
            .map(|&i| indices[i])
            .collect::<Vec<_>>();
        let extras = unsafe { NativeExtras::parse(info, &native_ordinals)? };
        entries.sort_unstable_by_key(|entry| Arc::as_ptr(&entry.swapchain) as usize);
        if entries
            .windows(2)
            .any(|pair| Arc::ptr_eq(&pair[0].swapchain, &pair[1].swapchain))
        {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        let mut owners = Vec::new();
        for entry in &entries {
            owners.push(
                entry
                    .swapchain
                    .live
                    .enter_until(AcquireDeadline::after(std::time::Duration::from_secs(2)))
                    .map_err(|_| vk::Result::ERROR_DEVICE_LOST)?,
            );
        }
        let bridges = (0..entries.len() + usize::from(!native_handles.is_empty()))
            .map(|_| Semaphore::new(&queue))
            .collect::<Result<Vec<_>, _>>()?;
        let mut native_work = bridges
            .get(entries.len())
            .map(|bridge| NativePresent::new(&device, native_handles.len(), bridge.clone()))
            .transpose()?;
        let waits = if info.wait_semaphore_count == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    info.p_wait_semaphores,
                    info.wait_semaphore_count as usize,
                )
            }
            .iter()
            .map(|&s| (s, 0))
            .collect::<Vec<_>>()
        };
        let signals = bridges.iter().map(|s| (s.handle, 0)).collect::<Vec<_>>();
        let _guard = queue.gate.lock();
        let _fanout = queue.submit(&[], &waits, &signals, vk::Fence::null(), bridges.clone())?;
        let mut results = vec![vk::Result::SUCCESS; handles.len()];
        for ((entry, state), dependency) in entries.iter().zip(&mut owners).zip(&bridges) {
            let display_at =
                (entry.swapchain.mode == vk::PresentModeKHR::FIFO).then_some(state.next_present);
            let lease = state
                .leases
                .get_mut(entry.index as usize)
                .and_then(Option::take);
            let result = match (state.controller.as_mut(), lease) {
                (Some(controller), Some(lease)) => {
                    let handoff = PresentInfo {
                        queue: queue.clone(),
                        dependency: dependency.clone(),
                        display_at,
                    };
                    if entry.swapchain.closed.load(Ordering::Acquire) {
                        controller.present_retired(lease, handoff)
                    } else {
                        controller.present(
                            lease,
                            handoff,
                            polymer_platform_windows::performance_counter().unwrap_or(0),
                        )
                    }
                    .map(|_| vk::Result::SUCCESS)
                    .unwrap_or_else(WsiResult::controller)
                }
                _ => vk::Result::ERROR_OUT_OF_DATE_KHR,
            };
            state.next_present = Instant::now() + entry.swapchain.period;
            results[entry.ordinal] = if matches!(
                result,
                vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
            ) {
                // The batch has already consumed the application waits.
                vk::Result::ERROR_DEVICE_LOST
            } else {
                result
            };
        }
        let mut native_aggregate = vk::Result::SUCCESS;
        if let Some(bridge) = bridges.get(entries.len()) {
            let mut native_results = vec![vk::Result::SUCCESS; native_handles.len()];
            let wait = [bridge.handle];
            let mut regions = vk::PresentRegionsKHR::default().regions(&extras.regions);
            let mut group = vk::DeviceGroupPresentInfoKHR::default()
                .device_masks(&extras.masks)
                .mode(vk::DeviceGroupPresentModeFlagsKHR::LOCAL);
            let work = native_work.as_mut().unwrap();
            let mut completion = vk::SwapchainPresentFenceInfoEXT::default().fences(&work.fences);
            let mut native_info = vk::PresentInfoKHR::default()
                .push_next(&mut completion)
                .wait_semaphores(&wait)
                .swapchains(&native_handles)
                .image_indices(&native_indices)
                .results(&mut native_results);
            if !extras.regions.is_empty() {
                native_info = native_info.push_next(&mut regions);
            }
            if !extras.masks.is_empty() {
                native_info = native_info.push_next(&mut group);
            }
            native_aggregate = unsafe { next(queue_handle, &native_info) };
            work.submitted = !matches!(
                native_aggregate,
                vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
            );
            if !work.submitted {
                native_aggregate = vk::Result::ERROR_DEVICE_LOST;
            }
            for (n, &ordinal) in native_ordinals.iter().enumerate() {
                results[ordinal] = native_results[n];
            }
        }
        if let Some(work) = native_work {
            device.native_presents.lock().push(work);
        }
        if native_aggregate == vk::Result::ERROR_DEVICE_LOST
            || results.contains(&vk::Result::ERROR_DEVICE_LOST)
        {
            device.lost.store(true, Ordering::Release);
        }
        if !info.p_results.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(results.as_ptr(), info.p_results, results.len());
            }
        }
        results.push(native_aggregate);
        Ok(Self::aggregate(&results))
    }
    fn aggregate(results: &[vk::Result]) -> vk::Result {
        for result in [
            vk::Result::ERROR_DEVICE_LOST,
            vk::Result::ERROR_SURFACE_LOST_KHR,
            vk::Result::ERROR_OUT_OF_DATE_KHR,
            vk::Result::ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT,
            vk::Result::SUBOPTIMAL_KHR,
        ] {
            if results.contains(&result) {
                return result;
            }
        }
        results
            .iter()
            .copied()
            .find(|result| *result != vk::Result::SUCCESS)
            .unwrap_or(vk::Result::SUCCESS)
    }
}

struct NativeExtras<'a> {
    regions: Vec<vk::PresentRegionKHR<'a>>,
    masks: Vec<u32>,
}
impl<'a> NativeExtras<'a> {
    unsafe fn parse(info: &vk::PresentInfoKHR<'a>, native: &[usize]) -> Result<Self, vk::Result> {
        let mut result = Self {
            regions: Vec::new(),
            masks: Vec::new(),
        };
        let mut current = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while let Some(base) = unsafe { current.as_ref() } {
            match base.s_type {
                vk::StructureType::PRESENT_REGIONS_KHR => {
                    let regions = unsafe { &*current.cast::<vk::PresentRegionsKHR<'_>>() };
                    let values = unsafe {
                        std::slice::from_raw_parts(
                            regions.p_regions,
                            regions.swapchain_count as usize,
                        )
                    };
                    result.regions = native.iter().map(|&i| values[i]).collect();
                }
                vk::StructureType::DEVICE_GROUP_PRESENT_INFO_KHR => {
                    let group = unsafe { &*current.cast::<vk::DeviceGroupPresentInfoKHR<'_>>() };
                    let values = unsafe {
                        std::slice::from_raw_parts(
                            group.p_device_masks,
                            group.swapchain_count as usize,
                        )
                    };
                    if group.mode != vk::DeviceGroupPresentModeFlagsKHR::LOCAL
                        || values.iter().any(|&mask| mask != 1)
                    {
                        return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
                    }
                    result.masks = native.iter().map(|&i| values[i]).collect();
                }
                _ => return Err(vk::Result::ERROR_EXTENSION_NOT_PRESENT),
            }
            current = base.p_next;
        }
        Ok(result)
    }
}
pub(super) unsafe extern "system" fn present(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    let result = match unsafe { PresentBatch::run(queue, &*info) } {
        Ok(result) | Err(result) => result,
    };
    if result == vk::Result::ERROR_DEVICE_LOST {
        if let Some(device) = Device::queue(queue) {
            device.lost.store(true, Ordering::Release);
        }
    }
    result
}
