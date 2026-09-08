//! WSI interception and immediate argument translation. No guest command stream
//! is retained: all command buffers, draws, and secondary execution stay native.
mod compatibility;
mod device;
pub(crate) mod features;
mod layouts;
mod present;
mod queues;
mod surface;
mod wsi;
use ash::{vk, vk::Handle};
use dashmap::DashMap;
pub(crate) use device::Device;
use polymer_presentation::swapchain::*;
use polymer_producer_vulkan::swapchain::VulkanVirtualBackend;
use std::sync::{Arc, LazyLock};
pub(crate) use surface::Surface;

static DEVICES: LazyLock<DashMap<vk::Device, Arc<Device>>> = LazyLock::new(DashMap::new);
static SURFACES: LazyLock<DashMap<vk::SurfaceKHR, Arc<Surface>>> = LazyLock::new(DashMap::new);
static SWAPCHAINS: LazyLock<DashMap<vk::SwapchainKHR, Arc<wsi::Swapchain>>> =
    LazyLock::new(DashMap::new);

pub(crate) fn enabled() -> bool {
    polymer_hook::config::render_configuration().is_ok()
}

pub(crate) fn device_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    if !enabled() {
        return None;
    }
    let pointer = match name {
        b"vkCreateSwapchainKHR" => wsi::create as *const (),
        b"vkDestroySwapchainKHR" => wsi::destroy as *const (),
        b"vkGetSwapchainImagesKHR" => wsi::images as *const (),
        b"vkAcquireNextImageKHR" => wsi::vkAcquireNextImageKHR as *const (),
        b"vkAcquireNextImage2KHR" => wsi::vkAcquireNextImage2KHR as *const (),
        b"vkQueuePresentKHR" => present::present as *const (),
        b"vkQueueSubmit" => queues::submit as *const (),
        b"vkQueueSubmit2" | b"vkQueueSubmit2KHR" => queues::submit2 as *const (),
        b"vkQueueBindSparse" => queues::vkQueueBindSparse as *const (),
        b"vkQueueWaitIdle" => queues::vkQueueWaitIdle as *const (),
        b"vkDeviceWaitIdle" => queues::vkDeviceWaitIdle as *const (),
        _ => {
            return layouts::hook(name).or_else(|| compatibility::Compatibility::device_hook(name));
        }
    };
    crate::hook(pointer)
}
pub(crate) fn instance_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    if !enabled() {
        return None;
    }
    if name == b"vkEnumerateDeviceExtensionProperties" {
        return crate::hook(features::vkEnumerateDeviceExtensionProperties as *const ());
    }
    surface::hook(name).or_else(|| compatibility::Compatibility::instance_hook(name))
}

impl Device {
    pub(super) fn get(handle: vk::Device) -> Result<Arc<Self>, vk::Result> {
        DEVICES
            .get(&handle)
            .map(|v| v.clone())
            .ok_or(vk::Result::ERROR_DEVICE_LOST)
    }
    pub(super) fn queue(queue: vk::Queue) -> Option<Arc<Self>> {
        let (_, handle) = crate::layer::queue_dispatch_data(queue)?;
        Self::get(handle).ok()
    }
}

impl wsi::Swapchain {
    pub(super) fn get(
        device: vk::Device,
        handle: vk::SwapchainKHR,
    ) -> Result<Option<Arc<Self>>, vk::Result> {
        let Some(value) = SWAPCHAINS.get(&handle).map(|v| v.clone()) else {
            return Ok(None);
        };
        if value.device.external.device.handle() != device {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        Ok(Some(value))
    }
}

pub(super) struct WsiResult;
impl WsiResult {
    fn controller(error: ControllerError<vk::Result>) -> vk::Result {
        match error {
            ControllerError::Backend(error) => error,
            ControllerError::WouldBlock => vk::Result::NOT_READY,
            ControllerError::Timeout => vk::Result::TIMEOUT,
            ControllerError::Contract(_) => vk::Result::ERROR_INITIALIZATION_FAILED,
            ControllerError::Transport(_) | ControllerError::Retiring(_) => {
                vk::Result::ERROR_OUT_OF_DATE_KHR
            }
        }
    }
    unsafe fn enumerate<T: Copy>(values: &[T], count: *mut u32, output: *mut T) -> vk::Result {
        if count.is_null() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        if output.is_null() {
            unsafe {
                *count = values.len() as u32;
            }
            return vk::Result::SUCCESS;
        }
        let copied = unsafe { (*count as usize).min(values.len()) };
        unsafe {
            std::ptr::copy_nonoverlapping(values.as_ptr(), output, copied);
            *count = copied as u32;
        }
        if copied < values.len() {
            vk::Result::INCOMPLETE
        } else {
            vk::Result::SUCCESS
        }
    }
}

#[cfg(test)]
mod gpu_tests;

pub(crate) use present::present as queue_present;
pub(crate) use wsi::{
    create as create_swapchain, destroy as destroy_swapchain, images as swapchain_images,
};
