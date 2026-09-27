use super::*;
use polymer_producer_vulkan::swapchain::{PresentInfo, native::Semaphore};
use std::{sync::atomic::Ordering, time::Instant};

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
        for (ordinal, (&handle, &index)) in handles.iter().zip(indices).enumerate() {
            let swapchain = wsi::Swapchain::get(device.external.device.handle(), handle)?
                .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
            entries.push(Entry {
                ordinal,
                index,
                swapchain,
            });
        }
        let queue = queue.ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
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
        let bridges = (0..entries.len())
            .map(|_| Semaphore::new(&queue))
            .collect::<Result<Vec<_>, _>>()?;
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
            let result = match result {
                vk::Result::SUCCESS if entry.swapchain.superseded() => vk::Result::SUBOPTIMAL_KHR,
                result => result,
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
        drop(owners);
        for entry in &entries {
            entry.swapchain.availability.notify();
        }
        if results.contains(&vk::Result::ERROR_DEVICE_LOST) {
            device.lost.store(true, Ordering::Release);
        }
        if !info.p_results.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(results.as_ptr(), info.p_results, results.len());
            }
        }
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

pub(crate) unsafe extern "system" fn present(
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
