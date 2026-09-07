use crate::ExternalDevice;
use ash::vk;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};

pub(crate) struct ExportImageRingDesc {
    pub extent: vk::Extent2D,
    pub format: vk::Format,
    pub slots: u32,
    pub usage: vk::ImageUsageFlags,
    pub queue_families: Vec<u32>,
}

/// Native Vulkan images importing the allocations shared with the consumer.
pub(crate) struct ExportImageRing {
    device: ash::Device,
    slots: Vec<ExportImageSlot>,
}

pub(crate) struct ExportImageSlot {
    image: vk::Image,
    memory: vk::DeviceMemory,
    handle: OwnedHandle,
    _resource: ID3D12Resource,
}

impl ExportImageRing {
    pub(crate) fn create(
        external: &ExternalDevice,
        desc: ExportImageRingDesc,
    ) -> Result<Self, vk::Result> {
        if desc.slots == 0 || desc.extent.width == 0 || desc.extent.height == 0 {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        let d3d12 = crate::d3d12_interop::create_device(external.adapter_luid)?;
        let mut ring = Self {
            device: external.device.clone(),
            slots: Vec::with_capacity(desc.slots as usize),
        };
        for _ in 0..desc.slots {
            ring.slots
                .push(ExportImageSlot::create(external, &d3d12, &desc)?);
        }
        Ok(ring)
    }

    pub(crate) fn slots(&self) -> &[ExportImageSlot] {
        &self.slots
    }
}

impl Drop for ExportImageRing {
    fn drop(&mut self) {
        for slot in self.slots.drain(..) {
            unsafe {
                self.device.destroy_image(slot.image, None);
                self.device.free_memory(slot.memory, None);
            }
        }
    }
}

impl ExportImageSlot {
    pub(crate) fn image(&self) -> vk::Image {
        self.image
    }
    pub(crate) fn raw_handle(&self) -> isize {
        self.handle.as_raw_handle() as isize
    }

    fn create(
        external: &ExternalDevice,
        d3d12: &ID3D12Device,
        desc: &ExportImageRingDesc,
    ) -> Result<Self, vk::Result> {
        let shared =
            crate::d3d12_interop::create_resource(d3d12, desc.extent, desc.format, desc.usage)?;
        let device = &external.device;
        let mut external_image = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::D3D12_RESOURCE);
        let info = vk::ImageCreateInfo::default()
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
            .usage(desc.usage)
            .sharing_mode(if desc.queue_families.len() > 1 {
                vk::SharingMode::CONCURRENT
            } else {
                vk::SharingMode::EXCLUSIVE
            })
            .queue_family_indices(&desc.queue_families)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { device.create_image(&info, None)? };
        let memory = match Self::import_memory(external, image, &shared.handle) {
            Ok(memory) => memory,
            Err(error) => {
                unsafe { device.destroy_image(image, None) };
                return Err(error);
            }
        };
        if let Err(error) = unsafe { device.bind_image_memory(image, memory, 0) } {
            unsafe {
                device.destroy_image(image, None);
                device.free_memory(memory, None);
            }
            return Err(error);
        }
        Ok(Self {
            image,
            memory,
            handle: shared.handle,
            _resource: shared.resource,
        })
    }

    fn import_memory(
        external: &ExternalDevice,
        image: vk::Image,
        handle: &OwnedHandle,
    ) -> Result<vk::DeviceMemory, vk::Result> {
        let device = &external.device;
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let mut properties = vk::MemoryWin32HandlePropertiesKHR::default();
        let result = unsafe {
            (external.get_memory_win32_handle_properties)(
                device.handle(),
                vk::ExternalMemoryHandleTypeFlags::D3D12_RESOURCE,
                handle.as_raw_handle() as _,
                &mut properties,
            )
        };
        if result != vk::Result::SUCCESS {
            return Err(result);
        }
        let allowed = requirements.memory_type_bits & properties.memory_type_bits;
        let memory_type = external.memory_properties.memory_types
            [..external.memory_properties.memory_type_count as usize]
            .iter()
            .enumerate()
            .find(|(index, ty)| {
                allowed & (1 << index) != 0
                    && ty
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .map(|(index, _)| index as u32)
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let mut import = vk::ImportMemoryWin32HandleInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::D3D12_RESOURCE)
            .handle(handle.as_raw_handle() as _);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let info = vk::MemoryAllocateInfo::default()
            .push_next(&mut import)
            .push_next(&mut dedicated)
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        unsafe { device.allocate_memory(&info, None) }
    }
}
