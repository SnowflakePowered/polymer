use super::*;
use polymer_producer_vulkan::virtual_swapchain::native::Queue;
use std::{ffi::CStr, sync::atomic::Ordering};

struct QueueDispatch {
    device: vk::Device,
    gdpa: vk::PFN_vkGetDeviceProcAddr,
    owner: Option<Arc<Queue>>,
}
impl QueueDispatch {
    fn new(queue: vk::Queue) -> Result<Self, vk::Result> {
        let (gdpa, device) =
            crate::layer::queue_dispatch_data(queue).ok_or(vk::Result::ERROR_DEVICE_LOST)?;
        let virtual_device = Device::get(device).ok();
        if virtual_device
            .as_ref()
            .is_some_and(|device| device.lost.load(Ordering::Acquire))
        {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        let owner = virtual_device.and_then(|device| device.queues.get(&queue.as_raw()).cloned());
        Ok(Self {
            device,
            gdpa,
            owner,
        })
    }
    unsafe fn invoke<T: Copy>(
        queue: vk::Queue,
        name: &CStr,
        alias: &CStr,
        call: impl FnOnce(T) -> vk::Result,
    ) -> vk::Result {
        let dispatch = match Self::new(queue) {
            Ok(dispatch) => dispatch,
            Err(error) => return error,
        };
        let _guard = dispatch.owner.as_ref().map(|queue| queue.gate.lock());
        let next = unsafe {
            crate::terminal::device_proc(dispatch.gdpa, dispatch.device, name)
                .or_else(|| crate::terminal::device_proc(dispatch.gdpa, dispatch.device, alias))
        };
        next.map_or(vk::Result::ERROR_FEATURE_NOT_PRESENT, call)
    }
}
pub(super) unsafe extern "system" fn submit(
    queue: vk::Queue,
    count: u32,
    info: *const vk::SubmitInfo<'_>,
    fence: vk::Fence,
) -> vk::Result {
    unsafe {
        QueueDispatch::invoke(
            queue,
            c"vkQueueSubmit",
            c"vkQueueSubmit",
            |next: vk::PFN_vkQueueSubmit| next(queue, count, info, fence),
        )
    }
}
pub(super) unsafe extern "system" fn submit2(
    queue: vk::Queue,
    count: u32,
    info: *const vk::SubmitInfo2<'_>,
    fence: vk::Fence,
) -> vk::Result {
    unsafe {
        QueueDispatch::invoke(
            queue,
            c"vkQueueSubmit2",
            c"vkQueueSubmit2KHR",
            |next: vk::PFN_vkQueueSubmit2| next(queue, count, info, fence),
        )
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkQueueBindSparse(
    queue: vk::Queue,
    count: u32,
    info: *const vk::BindSparseInfo<'_>,
    fence: vk::Fence,
) -> vk::Result {
    unsafe {
        QueueDispatch::invoke(
            queue,
            c"vkQueueBindSparse",
            c"vkQueueBindSparse",
            |next: vk::PFN_vkQueueBindSparse| next(queue, count, info, fence),
        )
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkQueueWaitIdle(queue: vk::Queue) -> vk::Result {
    let dispatch = match QueueDispatch::new(queue) {
        Ok(dispatch) => dispatch,
        Err(error) => return error,
    };
    if let Some(owner) = dispatch.owner {
        return owner
            .drain()
            .map_or_else(|error| error, |_| vk::Result::SUCCESS);
    }
    unsafe {
        QueueDispatch::invoke(
            queue,
            c"vkQueueWaitIdle",
            c"vkQueueWaitIdle",
            |next: vk::PFN_vkQueueWaitIdle| next(queue),
        )
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDeviceWaitIdle(handle: vk::Device) -> vk::Result {
    let device = Device::get(handle).ok();
    let guards = device.as_ref().map(|device| {
        device
            .queues
            .values()
            .map(|queue| queue.gate.lock())
            .collect::<Vec<_>>()
    });
    let next: Option<vk::PFN_vkDeviceWaitIdle> = crate::layer::device_gdpa(handle)
        .and_then(|g| unsafe { crate::terminal::device_proc(g, handle, c"vkDeviceWaitIdle") });
    let result = next.map_or(vk::Result::ERROR_DEVICE_LOST, |next| unsafe {
        next(handle)
    });
    drop(guards);
    if let Some(device) = device {
        device.poll_retired();
    }
    result
}
