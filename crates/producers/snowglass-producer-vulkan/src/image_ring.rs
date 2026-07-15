use ash::vk;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

#[derive(Clone, Copy)]
pub struct ExportImageRingDesc {
    pub extent: vk::Extent2D,
    pub format: vk::Format,
    pub slots: u32,
}

/// Dedicated, device-local images whose allocations can be imported by
/// another Windows process.
#[cfg(windows)]
pub struct ExportImageRing {
    device: ash::Device,
    slots: Vec<ExportImageSlot>,
    desc: ExportImageRingDesc,
}

#[cfg(windows)]
pub struct ExportImageSlot {
    image: vk::Image,
    memory: vk::DeviceMemory,
    handle: OwnedHandle,
}

#[cfg(windows)]
impl ExportImageSlot {
    pub fn image(&self) -> vk::Image {
        self.image
    }

    pub fn memory(&self) -> vk::DeviceMemory {
        self.memory
    }

    pub fn raw_handle(&self) -> isize {
        self.handle.as_raw_handle() as isize
    }
}

#[cfg(windows)]
impl ExportImageRing {
    /// Allocates exportable images and obtains one owned NT handle per slot.
    /// The device must have `VK_KHR_external_memory_win32` enabled.
    pub fn create(
        device: ash::Device,
        external_memory: &ash::khr::external_memory_win32::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        desc: ExportImageRingDesc,
    ) -> Result<Self, vk::Result> {
        Self::create_with_function(
            device,
            external_memory.fp().get_memory_win32_handle_khr,
            memory_properties,
            desc,
        )
    }

    /// Layer-friendly constructor for callers that load extension functions
    /// directly through the next device dispatch table.
    pub fn create_with_function(
        device: ash::Device,
        get_memory_win32_handle: vk::PFN_vkGetMemoryWin32HandleKHR,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        desc: ExportImageRingDesc,
    ) -> Result<Self, vk::Result> {
        if desc.slots == 0 || desc.extent.width == 0 || desc.extent.height == 0 {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }

        let mut ring = Self {
            device,
            slots: Vec::with_capacity(desc.slots as usize),
            desc,
        };
        for _ in 0..desc.slots {
            ring.slots.push(unsafe {
                create_export_image(
                    &ring.device,
                    get_memory_win32_handle,
                    memory_properties,
                    desc,
                )?
            });
        }
        Ok(ring)
    }

    pub fn desc(&self) -> ExportImageRingDesc {
        self.desc
    }

    pub fn slots(&self) -> &[ExportImageSlot] {
        &self.slots
    }
}

#[cfg(windows)]
impl Drop for ExportImageRing {
    fn drop(&mut self) {
        unsafe {
            for slot in self.slots.drain(..) {
                self.device.destroy_image(slot.image, None);
                self.device.free_memory(slot.memory, None);
            }
        }
    }
}

#[cfg(windows)]
unsafe fn create_export_image(
    device: &ash::Device,
    get_memory_win32_handle: vk::PFN_vkGetMemoryWin32HandleKHR,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    desc: ExportImageRingDesc,
) -> Result<ExportImageSlot, vk::Result> {
    let mut external_image = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
    let image_info = vk::ImageCreateInfo::default()
        .push_next(&mut external_image)
        .image_type(vk::ImageType::TYPE_2D)
        .format(desc.format)
        .extent(vk::Extent3D {
            width: desc.extent.width,
            height: desc.extent.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { device.create_image(&image_info, None)? };
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let Some(memory_type_index) = find_memory_type(
        memory_properties,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ) else {
        unsafe { device.destroy_image(image, None) };
        return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
    };
    let mut export_info = vk::ExportMemoryAllocateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
    let allocation_info = vk::MemoryAllocateInfo::default()
        .push_next(&mut export_info)
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index);
    let memory = match unsafe { device.allocate_memory(&allocation_info, None) } {
        Ok(memory) => memory,
        Err(error) => {
            unsafe { device.destroy_image(image, None) };
            return Err(error);
        }
    };
    if let Err(error) = unsafe { device.bind_image_memory(image, memory, 0) } {
        unsafe {
            device.free_memory(memory, None);
            device.destroy_image(image, None);
        }
        return Err(error);
    }
    let handle_info = vk::MemoryGetWin32HandleInfoKHR::default()
        .memory(memory)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
    let mut raw_handle = 0;
    let handle_result =
        unsafe { get_memory_win32_handle(device.handle(), &handle_info, &mut raw_handle) };
    match handle_result {
        vk::Result::SUCCESS if raw_handle != 0 => {}
        vk::Result::SUCCESS => {
            unsafe {
                device.destroy_image(image, None);
                device.free_memory(memory, None);
            }
            return Err(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE);
        }
        error => {
            unsafe {
                device.destroy_image(image, None);
                device.free_memory(memory, None);
            }
            return Err(error);
        }
    }
    // SAFETY: Vulkan returned a new owned Win32 HANDLE for the caller.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw_handle as _) };
    Ok(ExportImageSlot {
        image,
        memory,
        handle,
    })
}

fn find_memory_type(
    properties: &vk::PhysicalDeviceMemoryProperties,
    allowed_bits: u32,
    required: vk::MemoryPropertyFlags,
) -> Option<u32> {
    properties.memory_types[..properties.memory_type_count as usize]
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            allowed_bits & (1 << index) != 0 && memory_type.property_flags.contains(required)
        })
        .map(|(index, _)| index as u32)
}
