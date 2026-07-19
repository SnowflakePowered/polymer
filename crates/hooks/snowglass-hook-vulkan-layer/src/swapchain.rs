//! Swapchain lifecycle and presentation observers.
//!
//! Only raw argument parsing and dispatch lookups happen here; the capture
//! orchestration (streams, transports, copies) lives in
//! `snowglass_producer_vulkan::capture`.

use crate::layer::*;
use ash::vk;
use snowglass_hook::HookChain;
use snowglass_producer_vulkan::capture;

pub(crate) fn destroy_device(device: vk::Device) {
    capture::device_destroyed(device);
}

pub(crate) fn create_win32_surface(
    create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>,
    surface: *mut vk::SurfaceKHR,
    result: vk::Result,
) {
    if result == vk::Result::SUCCESS
        && !create_info.is_null()
        && !surface.is_null()
        && unsafe { *surface } != vk::SurfaceKHR::null()
    {
        capture::surface_created(unsafe { *surface }, unsafe { (*create_info).hwnd as isize });
    }
}

pub(crate) fn destroy_surface(surface: vk::SurfaceKHR) {
    capture::surface_destroyed(surface);
}

pub(crate) fn create_swapchain(
    device: vk::Device,
    create_info: *const vk::SwapchainCreateInfoKHR<'_>,
    swapchain: *mut vk::SwapchainKHR,
    result: vk::Result,
) {
    if result != vk::Result::SUCCESS || create_info.is_null() || swapchain.is_null() {
        return;
    }
    let dispatch =
        unsafe { dispatch_key(device) }.and_then(|key| DEVICE.get(&key).map(|entry| entry.clone()));
    let info = unsafe { &*create_info };
    capture::swapchain_created(
        unsafe { *swapchain },
        device,
        dispatch.and_then(|dispatch| dispatch.capture),
        info.image_extent,
        info.image_format,
        info.surface,
    );
}

pub(crate) fn destroy_swapchain(swapchain: vk::SwapchainKHR) {
    capture::swapchain_destroyed(swapchain);
}

pub(crate) fn get_swapchain_images(
    swapchain: vk::SwapchainKHR,
    image_count: *mut u32,
    images: *mut vk::Image,
    result: vk::Result,
) {
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !image_count.is_null()
        && !images.is_null()
    {
        let images = unsafe { std::slice::from_raw_parts(images, *image_count as usize) };
        capture::swapchain_images(swapchain, images);
    }
}

pub(crate) fn queue_submit(
    queue: vk::Queue,
    submit_count: u32,
    submits: *const vk::SubmitInfo<'_>,
    result: vk::Result,
) {
    let Some(queue_state) = ({ QUEUE.get(&queue).map(|entry| entry.clone()) }) else {
        return;
    };
    if result != vk::Result::SUCCESS || submit_count == 0 || submits.is_null() {
        return;
    }
    let mut command_buffers = Vec::new();
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
    capture::tracked_submission(queue, queue_state.queue_family_index, &command_buffers);
}

pub(crate) unsafe fn handle_queue_present(
    queue: vk::Queue,
    present_info: *const vk::PresentInfoKHR<'_>,
    mut next: crate::hooks::QueuePresentContext<'_>,
) -> vk::Result {
    let continuation = *next
        .fp_next()
        .expect("Vulkan around chains always have a terminal");
    let Some(queue_state) = ({ QUEUE.get(&queue).map(|entry| entry.clone()) }) else {
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
    let swapchains =
        unsafe { std::slice::from_raw_parts(original.p_swapchains, original.swapchain_count as usize) };
    let image_indices = unsafe {
        std::slice::from_raw_parts(original.p_image_indices, original.swapchain_count as usize)
    };
    let presents = swapchains
        .iter()
        .copied()
        .zip(image_indices.iter().copied())
        .collect::<Vec<_>>();
    match capture::present(queue, queue_state.queue_family_index, &presents, waits) {
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
