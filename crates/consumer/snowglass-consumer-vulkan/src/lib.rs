//! Vulkan external-resource import and synchronization backend.

use ash::{Entry, vk};
use snowglass_ipc::StreamReady;
use snowglass_protocol::{ExternalImageKind, ExternalSyncKind};
use std::{ffi::CStr, fmt};

pub const BACKEND_NAME: &str = "vulkan";

fn format_for_stream(stream: &StreamReady) -> Result<vk::Format, ImportError> {
    let format = match stream.image_kind {
        ExternalImageKind::VulkanOpaqueWin32 => vk::Format::from_raw(stream.format as i32),
        ExternalImageKind::D3d11SharedResourceNt => match stream.format {
            10 => vk::Format::R16G16B16A16_SFLOAT,
            24 => vk::Format::A2B10G10R10_UNORM_PACK32,
            28 => vk::Format::R8G8B8A8_UNORM,
            29 => vk::Format::R8G8B8A8_SRGB,
            87 => vk::Format::B8G8R8A8_UNORM,
            91 => vk::Format::B8G8R8A8_SRGB,
            _ => return Err(ImportError::InvalidStream("unsupported DXGI format")),
        },
        _ => {
            return Err(ImportError::InvalidStream(
                "unsupported external image kind",
            ));
        }
    };
    Ok(format)
}

/// Imported capture resources that use a caller-owned Vulkan device. This is
/// the direct-composition path used by the viewer swapchain.
pub struct ImportedImageRing {
    device: ash::Device,
    images: Vec<vk::Image>,
    memories: Vec<vk::DeviceMemory>,
    timeline: vk::Semaphore,
    image_kind: ExternalImageKind,
    pub width: u32,
    pub height: u32,
    pub format: vk::Format,
}

impl ImportedImageRing {
    pub fn import_into(
        instance: &ash::Instance,
        device: ash::Device,
        physical_device: vk::PhysicalDevice,
        stream: &StreamReady,
    ) -> Result<Self, ImportError> {
        if stream.slot_count == 0 || stream.slot_count as usize > stream.image_handles.len() {
            return Err(ImportError::InvalidStream("invalid slot count"));
        }
        // SAFETY: the caller-owned instance/device are live, and this object
        // owns every imported child resource until Drop.
        let (images, memories, timeline) =
            unsafe { import_resources(instance, &device, physical_device, stream)? };
        Ok(Self {
            device,
            images,
            memories,
            timeline,
            image_kind: stream.image_kind,
            width: stream.width,
            height: stream.height,
            format: format_for_stream(stream)?,
        })
    }

    pub fn image(&self, slot: u32) -> Option<vk::Image> {
        self.images.get(slot as usize).copied()
    }

    pub fn timeline(&self) -> vk::Semaphore {
        self.timeline
    }

    pub fn memory(&self, slot: u32) -> Option<vk::DeviceMemory> {
        self.memories.get(slot as usize).copied()
    }

    pub fn uses_keyed_mutex(&self) -> bool {
        self.image_kind == ExternalImageKind::D3d11SharedResourceNt
    }

    pub fn wait_for(&self, value: u64, timeout_ns: u64) -> Result<(), vk::Result> {
        let semaphores = [self.timeline];
        let values = [value];
        let wait = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        unsafe { self.device.wait_semaphores(&wait, timeout_ns) }
    }
}

impl Drop for ImportedImageRing {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_semaphore(self.timeline, None);
            for image in self.images.drain(..) {
                self.device.destroy_image(image, None);
            }
            for memory in self.memories.drain(..) {
                self.device.free_memory(memory, None);
            }
        }
    }
}

#[derive(Debug)]
pub enum ImportError {
    Loader(ash::LoadingError),
    Vulkan(vk::Result),
    NoCompatibleDevice,
    InvalidStream(&'static str),
}

impl fmt::Display for ImportError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader(error) => write!(output, "could not load Vulkan: {error}"),
            Self::Vulkan(error) => write!(output, "Vulkan import failed: {error}"),
            Self::NoCompatibleDevice => write!(output, "no compatible Vulkan device was found"),
            Self::InvalidStream(message) => write!(output, "invalid stream: {message}"),
        }
    }
}

impl std::error::Error for ImportError {}
impl From<ash::LoadingError> for ImportError {
    fn from(value: ash::LoadingError) -> Self {
        Self::Loader(value)
    }
}
impl From<vk::Result> for ImportError {
    fn from(value: vk::Result) -> Self {
        Self::Vulkan(value)
    }
}

pub struct ImportedStream {
    instance: ash::Instance,
    device: ash::Device,
    images: Vec<vk::Image>,
    memories: Vec<vk::DeviceMemory>,
    timeline: vk::Semaphore,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    readbacks: Vec<ReadbackSlot>,
    width: u32,
    height: u32,
    format: vk::Format,
    image_kind: ExternalImageKind,
    pub physical_device: vk::PhysicalDevice,
    pub queue_family_index: u32,
}

struct ReadbackSlot {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    command: vk::CommandBuffer,
    fence: vk::Fence,
}

impl ImportedStream {
    pub fn import(stream: &StreamReady) -> Result<Self, ImportError> {
        if stream.slot_count == 0 || stream.slot_count as usize > stream.image_handles.len() {
            return Err(ImportError::InvalidStream("invalid slot count"));
        }
        if stream.timeline_semaphore_handle == 0 {
            return Err(ImportError::InvalidStream(
                "timeline semaphore handle is missing",
            ));
        }
        // SAFETY: all Vulkan objects are owned by the returned stream and are
        // destroyed in reverse order by Drop.
        unsafe { Self::import_unsafe(stream) }
    }

    unsafe fn import_unsafe(stream: &StreamReady) -> Result<Self, ImportError> {
        let entry = unsafe { Entry::load()? };
        let application = vk::ApplicationInfo::default()
            .application_name(c"snowglass-viewer")
            .api_version(vk::API_VERSION_1_2);
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&application),
                None,
            )?
        };
        let result = unsafe { Self::create_on_instance(&instance, stream) };
        match result {
            Ok((
                device,
                images,
                memories,
                timeline,
                physical_device,
                queue_family_index,
                queue,
                command_pool,
                readbacks,
            )) => Ok(Self {
                instance,
                device,
                images,
                memories,
                timeline,
                queue,
                command_pool,
                readbacks,
                width: stream.width,
                height: stream.height,
                format: format_for_stream(stream)?,
                image_kind: stream.image_kind,
                physical_device,
                queue_family_index,
            }),
            Err(error) => {
                unsafe { instance.destroy_instance(None) };
                Err(error)
            }
        }
    }

    unsafe fn create_on_instance(
        instance: &ash::Instance,
        stream: &StreamReady,
    ) -> Result<
        (
            ash::Device,
            Vec<vk::Image>,
            Vec<vk::DeviceMemory>,
            vk::Semaphore,
            vk::PhysicalDevice,
            u32,
            vk::Queue,
            vk::CommandPool,
            Vec<ReadbackSlot>,
        ),
        ImportError,
    > {
        let d3d11 = stream.image_kind == ExternalImageKind::D3d11SharedResourceNt;
        let (physical_device, queue_family_index) =
            unsafe { select_device(instance, d3d11).ok_or(ImportError::NoCompatibleDevice)? };
        let mut extensions = vec![
            ash::khr::external_memory_win32::NAME.as_ptr(),
            ash::khr::external_semaphore_win32::NAME.as_ptr(),
            ash::khr::timeline_semaphore::NAME.as_ptr(),
        ];
        if d3d11 {
            extensions.push(ash::khr::win32_keyed_mutex::NAME.as_ptr());
        }
        let priorities = [1.0];
        let queue = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&priorities);
        let mut timeline_features =
            vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true);
        let create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue))
            .enabled_extension_names(&extensions)
            .push_next(&mut timeline_features);
        let device = unsafe { instance.create_device(physical_device, &create_info, None)? };
        let result = unsafe { import_resources(instance, &device, physical_device, stream) };
        match result {
            Ok((images, memories, timeline)) => {
                let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
                let (command_pool, readbacks) = unsafe {
                    create_readbacks(
                        instance,
                        &device,
                        physical_device,
                        queue_family_index,
                        &images,
                        stream.width,
                        stream.height,
                    )?
                };
                Ok((
                    device,
                    images,
                    memories,
                    timeline,
                    physical_device,
                    queue_family_index,
                    queue,
                    command_pool,
                    readbacks,
                ))
            }
            Err(error) => {
                unsafe { device.destroy_device(None) };
                Err(ImportError::Vulkan(error))
            }
        }
    }

    pub fn wait_for(&self, value: u64, timeout_ns: u64) -> Result<(), vk::Result> {
        let semaphores = [self.timeline];
        let values = [value];
        let wait = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        unsafe { self.device.wait_semaphores(&wait, timeout_ns) }
    }

    pub fn completed_value(&self) -> Result<u64, vk::Result> {
        unsafe { self.device.get_semaphore_counter_value(self.timeline) }
    }

    /// Validation renderer path: waits for a captured slot, copies it to
    /// host-visible memory, and converts its packed pixels to RGBA8.
    pub fn read_frame_rgba(&mut self, slot: u32, value: u64) -> Result<Vec<u8>, vk::Result> {
        self.wait_for(value, 1_000_000_000)?;
        let readback = self
            .readbacks
            .get(slot as usize)
            .ok_or(vk::Result::ERROR_UNKNOWN)?;
        let byte_len = self.width as usize * self.height as usize * 4;
        unsafe {
            self.device
                .wait_for_fences(&[readback.fence], true, u64::MAX)?;
            self.device.reset_fences(&[readback.fence])?;
            let commands = [readback.command];
            let acquire_syncs = [self.memories[slot as usize]];
            let acquire_keys = [1];
            let acquire_timeouts = [u32::MAX];
            let release_syncs = acquire_syncs;
            let release_keys = [0];
            let mut keyed_mutex = vk::Win32KeyedMutexAcquireReleaseInfoKHR::default()
                .acquire_syncs(&acquire_syncs)
                .acquire_keys(&acquire_keys)
                .acquire_timeouts(&acquire_timeouts)
                .release_syncs(&release_syncs)
                .release_keys(&release_keys);
            let mut submit = vk::SubmitInfo::default().command_buffers(&commands);
            if self.image_kind == ExternalImageKind::D3d11SharedResourceNt {
                submit = submit.push_next(&mut keyed_mutex);
            }
            self.device
                .queue_submit(self.queue, &[submit], readback.fence)?;
            self.device
                .wait_for_fences(&[readback.fence], true, u64::MAX)?;
            let mapped = self.device.map_memory(
                readback.memory,
                0,
                byte_len as u64,
                vk::MemoryMapFlags::empty(),
            )?;
            let packed = std::slice::from_raw_parts(mapped.cast::<u8>(), byte_len);
            let rgba = convert_to_rgba(self.format, packed);
            self.device.unmap_memory(readback.memory);
            Ok(rgba)
        }
    }

    pub fn images(&self) -> &[vk::Image] {
        &self.images
    }
    pub fn device(&self) -> &ash::Device {
        &self.device
    }
}

impl Drop for ImportedStream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            for readback in self.readbacks.drain(..) {
                self.device.destroy_fence(readback.fence, None);
                self.device.destroy_buffer(readback.buffer, None);
                self.device.free_memory(readback.memory, None);
            }
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_semaphore(self.timeline, None);
            for image in self.images.drain(..) {
                self.device.destroy_image(image, None);
            }
            for memory in self.memories.drain(..) {
                self.device.free_memory(memory, None);
            }
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

unsafe fn select_device(
    instance: &ash::Instance,
    d3d11: bool,
) -> Option<(vk::PhysicalDevice, u32)> {
    let mut required = vec![
        ash::khr::external_memory_win32::NAME,
        ash::khr::external_semaphore_win32::NAME,
        ash::khr::timeline_semaphore::NAME,
    ];
    if d3d11 {
        required.push(ash::khr::win32_keyed_mutex::NAME);
    }
    unsafe { instance.enumerate_physical_devices().ok()? }
        .into_iter()
        .find_map(|physical| {
            let extensions = unsafe {
                instance
                    .enumerate_device_extension_properties(physical)
                    .ok()?
            };
            if !required.iter().all(|name| {
                extensions.iter().any(|property| {
                    (unsafe { CStr::from_ptr(property.extension_name.as_ptr()) }) == *name
                })
            }) {
                return None;
            }
            unsafe { instance.get_physical_device_queue_family_properties(physical) }
                .iter()
                .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .map(|index| (physical, index as u32))
        })
}

unsafe fn create_readbacks(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    queue_family_index: u32,
    images: &[vk::Image],
    width: u32,
    height: u32,
) -> Result<(vk::CommandPool, Vec<ReadbackSlot>), vk::Result> {
    let properties = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default().queue_family_index(queue_family_index),
            None,
        )?
    };
    let commands = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(images.len() as u32),
        )?
    };
    let size = width as u64 * height as u64 * 4;
    let mut slots = Vec::with_capacity(images.len());
    for (&image, &command) in images.iter().zip(&commands) {
        let buffer = unsafe {
            device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_DST)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?
        };
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let memory_type = properties.memory_types[..properties.memory_type_count as usize]
            .iter()
            .enumerate()
            .find(|(index, memory)| {
                requirements.memory_type_bits & (1 << index) != 0
                    && memory.property_flags.contains(
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )
            })
            .map(|(index, _)| index as u32)
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )?
        };
        unsafe { device.bind_buffer_memory(buffer, memory, 0)? };
        let fence = unsafe {
            device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?
        };
        unsafe {
            device.begin_command_buffer(command, &vk::CommandBufferBeginInfo::default())?;
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let to_source = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(image)
                .subresource_range(range);
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_source],
            );
            device.cmd_copy_image_to_buffer(
                command,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    })],
            );
            let to_general = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .image(image)
                .subresource_range(range);
            let to_host = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ)
                .buffer(buffer)
                .offset(0)
                .size(size);
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[to_host],
                &[to_general],
            );
            device.end_command_buffer(command)?;
        }
        slots.push(ReadbackSlot {
            buffer,
            memory,
            command,
            fence,
        });
    }
    Ok((pool, slots))
}

fn convert_to_rgba(format: vk::Format, packed: &[u8]) -> Vec<u8> {
    if format == vk::Format::A2B10G10R10_UNORM_PACK32 {
        let mut output = Vec::with_capacity(packed.len());
        for bytes in packed.chunks_exact(4) {
            let pixel = u32::from_le_bytes(bytes.try_into().expect("four-byte pixel"));
            output.push(((pixel & 0x3ff) * 255 / 1023) as u8);
            output.push((((pixel >> 10) & 0x3ff) * 255 / 1023) as u8);
            output.push((((pixel >> 20) & 0x3ff) * 255 / 1023) as u8);
            output.push(255);
        }
        output
    } else if matches!(
        format,
        vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB
    ) {
        packed
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect()
    } else {
        packed.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a2b10g10r10_to_rgba8() {
        let pixel = 1023u32 | (512 << 10) | (0 << 20) | (3 << 30);
        assert_eq!(
            convert_to_rgba(vk::Format::A2B10G10R10_UNORM_PACK32, &pixel.to_le_bytes()),
            [255, 127, 0, 255]
        );
    }
}

unsafe fn import_resources(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    stream: &StreamReady,
) -> Result<(Vec<vk::Image>, Vec<vk::DeviceMemory>, vk::Semaphore), vk::Result> {
    let format = format_for_stream(stream).map_err(|_| vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?;
    let memory_handle_type = match stream.image_kind {
        ExternalImageKind::VulkanOpaqueWin32 => vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32,
        ExternalImageKind::D3d11SharedResourceNt => {
            vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE
        }
        _ => return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED),
    };
    let semaphore_handle_type = match stream.sync_kind {
        ExternalSyncKind::VulkanTimelineSemaphore => {
            vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_WIN32
        }
        ExternalSyncKind::D3d11Fence => vk::ExternalSemaphoreHandleTypeFlags::D3D11_FENCE,
        _ => return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT),
    };
    let memory_properties =
        unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let mut images = Vec::new();
    let mut memories = Vec::new();
    for raw_handle in stream.image_handles.iter().take(stream.slot_count as usize) {
        let mut external =
            vk::ExternalMemoryImageCreateInfo::default().handle_types(memory_handle_type);
        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut external)
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: stream.width,
                height: stream.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { device.create_image(&image_info, None)? };
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let memory_type_index = memory_properties.memory_types
            [..memory_properties.memory_type_count as usize]
            .iter()
            .enumerate()
            .find(|(index, _)| requirements.memory_type_bits & (1 << index) != 0)
            .map(|(index, _)| index as u32)
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let mut import = vk::ImportMemoryWin32HandleInfoKHR::default()
            .handle_type(memory_handle_type)
            .handle(*raw_handle as isize);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let allocation = vk::MemoryAllocateInfo::default()
            .push_next(&mut import)
            .push_next(&mut dedicated)
            .allocation_size(requirements.size)
            .memory_type_index(memory_type_index);
        let memory = unsafe { device.allocate_memory(&allocation, None)? };
        unsafe { device.bind_image_memory(image, memory, 0)? };
        images.push(image);
        memories.push(memory);
    }
    let mut timeline_type =
        vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
    let semaphore = unsafe {
        device.create_semaphore(
            &vk::SemaphoreCreateInfo::default().push_next(&mut timeline_type),
            None,
        )?
    };
    let importer = ash::khr::external_semaphore_win32::Device::new(instance, device);
    let import = vk::ImportSemaphoreWin32HandleInfoKHR::default()
        .semaphore(semaphore)
        .handle_type(semaphore_handle_type)
        .handle(stream.timeline_semaphore_handle as isize);
    unsafe { importer.import_semaphore_win32_handle(&import)? };
    Ok((images, memories, semaphore))
}
