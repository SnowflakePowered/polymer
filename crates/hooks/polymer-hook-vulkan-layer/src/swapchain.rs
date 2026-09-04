//! Swapchain lifecycle and presentation observers.
//!
//! Only raw argument parsing and dispatch lookups happen here; the capture
//! orchestration (streams, transports, copies) lives in
//! `polymer_producer_vulkan::capture`.

use crate::layer::*;
use ash::vk;
use polymer_hook::HookChain;
use std::cell::RefCell;

thread_local! {
    static SUBMITTED_COMMAND_BUFFERS: RefCell<Vec<vk::CommandBuffer>> =
        const { RefCell::new(Vec::new()) };
}
pub(crate) fn create_win32_surface(
    instance: vk::Instance,
    create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>,
    surface: *mut vk::SurfaceKHR,
    result: vk::Result,
    context: &crate::hooks::CreateWin32SurfaceContext,
) {
    if result == vk::Result::SUCCESS
        && !create_info.is_null()
        && !surface.is_null()
        && unsafe { *surface } != vk::SurfaceKHR::null()
    {
        context
            .common
            .instance_capture(instance)
            .surface_created(unsafe { *surface }, unsafe { (*create_info).hwnd as isize });
    }
}

pub(crate) fn destroy_surface(
    instance: vk::Instance,
    surface: vk::SurfaceKHR,
    context: &crate::hooks::DestroySurfaceContext,
) {
    context
        .common
        .instance_capture(instance)
        .surface_destroyed(surface);
}

pub(crate) fn create_swapchain(
    device: vk::Device,
    create_info: *const vk::SwapchainCreateInfoKHR<'_>,
    swapchain: *mut vk::SwapchainKHR,
    result: vk::Result,
    context: &crate::hooks::CreateSwapchainContext,
) {
    if result != vk::Result::SUCCESS || create_info.is_null() || swapchain.is_null() {
        return;
    }
    let dispatch = unsafe { dispatch_key(device) }
        .and_then(|key| hook_state().device.get(&key).map(|entry| entry.clone()));
    let info = unsafe { &*create_info };
    context.common.device_capture(device).swapchain_created(
        unsafe { *swapchain },
        device,
        dispatch.and_then(|dispatch| dispatch.capture),
        context.common.device_tracking(device),
        info.image_extent,
        info.image_format,
        info.surface,
    );
}

pub(crate) fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    context: &crate::hooks::DestroySwapchainContext,
) {
    context
        .common
        .device_capture(device)
        .swapchain_destroyed(swapchain);
}

pub(crate) fn get_swapchain_images(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    image_count: *mut u32,
    images: *mut vk::Image,
    result: vk::Result,
    context: &crate::hooks::GetSwapchainImagesContext,
) {
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !image_count.is_null()
        && !images.is_null()
    {
        let images = unsafe { std::slice::from_raw_parts(images, *image_count as usize) };
        context
            .common
            .device_capture(device)
            .swapchain_images(swapchain, images);
    }
}

pub(crate) fn queue_submit(
    queue: vk::Queue,
    submit_count: u32,
    submits: *const vk::SubmitInfo<'_>,
    result: vk::Result,
    context: &crate::hooks::QueueSubmitContext,
) {
    if !polymer_hook::config::tracking_enabled() {
        return;
    }
    let Some(queue_state) = ({ hook_state().queue.get(&queue).map(|entry| entry.clone()) }) else {
        return;
    };
    if result != vk::Result::SUCCESS || submit_count == 0 || submits.is_null() {
        return;
    }
    SUBMITTED_COMMAND_BUFFERS.with_borrow_mut(|command_buffers| {
        command_buffers.clear();
        for submit in unsafe { std::slice::from_raw_parts(submits, submit_count as usize) } {
            if submit.command_buffer_count == 0 || submit.p_command_buffers.is_null() {
                continue;
            }
            command_buffers.extend_from_slice(unsafe {
                std::slice::from_raw_parts(
                    submit.p_command_buffers,
                    submit.command_buffer_count as usize,
                )
            });
        }
        context.common.queue_capture(queue).tracked_submission(
            &context.common.queue_tracking(queue),
            queue,
            queue_state.queue_family_index,
            command_buffers,
        );
    });
}

pub(crate) unsafe fn handle_queue_present(
    queue: vk::Queue,
    present_info: *const vk::PresentInfoKHR<'_>,
    mut next: crate::hooks::QueuePresentContext<'_>,
) -> vk::Result {
    let continuation = *next
        .fp_next()
        .expect("Vulkan around chains always have a terminal");
    let Some(queue_state) = ({ hook_state().queue.get(&queue).map(|entry| entry.clone()) }) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    if present_info.is_null()
        || unsafe { (*present_info).swapchain_count } == 0
        || unsafe { (*present_info).p_swapchains }.is_null()
        || unsafe { (*present_info).p_image_indices }.is_null()
    {
        return unsafe { continuation(queue, present_info, next) };
    }
    let original = unsafe { &*present_info };
    let waits = if original.wait_semaphore_count == 0 {
        &[]
    } else {
        unsafe {
            std::slice::from_raw_parts(
                original.p_wait_semaphores,
                original.wait_semaphore_count as usize,
            )
        }
    };
    let swapchains = unsafe {
        std::slice::from_raw_parts(original.p_swapchains, original.swapchain_count as usize)
    };
    let image_indices = unsafe {
        std::slice::from_raw_parts(original.p_image_indices, original.swapchain_count as usize)
    };
    let presents = swapchains
        .iter()
        .copied()
        .zip(image_indices.iter().copied())
        .collect::<Vec<_>>();
    match next.common.queue_capture(queue).present(
        &next.common.queue_tracking(queue),
        queue,
        queue_state.queue_family_index,
        &presents,
        waits,
    ) {
        Some(chained_waits) => {
            // The final copy-ready semaphore represents completion of every
            // captured swapchain and replaces the application's waits for
            // the combined present operation.
            let mut modified = *original;
            modified.wait_semaphore_count = chained_waits.len() as u32;
            modified.p_wait_semaphores = chained_waits.as_ptr();
            unsafe { continuation(queue, &modified, next) }
        }
        None => unsafe { continuation(queue, present_info, next) },
    }
}
