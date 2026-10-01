//! Vulkan external-resource import and synchronization backend.

use ash::vk;
use polymer_ipc::StreamReady;
use polymer_protocol::{AdapterLuid, ExternalImageKind, ExternalSyncKind, GraphicsHook};
use std::fmt;

pub const BACKEND_NAME: &str = "vulkan";

pub fn format_for_stream(stream: &StreamReady) -> Result<vk::Format, ImportError> {
    let format = match stream.image_kind {
        ExternalImageKind::D3d11Resource | ExternalImageKind::D3d12Resource => match stream.format {
            10 => vk::Format::R16G16B16A16_SFLOAT,
            24 => vk::Format::A2B10G10R10_UNORM_PACK32,
            28 => vk::Format::R8G8B8A8_UNORM,
            29 => vk::Format::R8G8B8A8_SRGB,
            87 => vk::Format::B8G8R8A8_UNORM,
            91 => vk::Format::B8G8R8A8_SRGB,
            _ => return Err(ImportError::InvalidStream("unsupported DXGI format")),
        },
        ExternalImageKind::IOSurface => {
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
    consumer_timeline: Option<vk::Semaphore>,
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
        validate_consumer_semaphore(stream)?;
        if let Some(adapter_luid) = requested_adapter(stream)
            && !physical_device_matches_luid(instance, physical_device, adapter_luid)
        {
            return Err(ImportError::NoCompatibleDevice);
        }
        // SAFETY: the caller-owned instance/device are live, and this object
        // owns every imported child resource until Drop.
        let (images, memories, timeline, consumer_timeline) =
            unsafe { import_resources(instance, &device, physical_device, stream)? };
        Ok(Self {
            device,
            images,
            memories,
            timeline,
            consumer_timeline,
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

    pub fn consumer_timeline(&self) -> Option<vk::Semaphore> {
        self.consumer_timeline
    }

    /// The caller must finish all GPU accesses and release external ownership first.
    pub fn release_all_idle(&self, last_received: u64) -> Result<(), vk::Result> {
        let Some(semaphore) = self.consumer_timeline else {
            return Ok(());
        };
        let current = unsafe { self.device.get_semaphore_counter_value(semaphore)? };
        let produced = unsafe { self.device.get_semaphore_counter_value(self.timeline)? };
        // Cover the bounded ring, including captures still queued on the producer.
        // A near-u64::MAX sentinel exceeds Vulkan's permitted timeline difference.
        let value = produced
            .max(last_received)
            .saturating_add(self.images.len() as u64);
        if value > current {
            unsafe {
                self.device.signal_semaphore(
                    &vk::SemaphoreSignalInfo::default()
                        .semaphore(semaphore)
                        .value(value),
                )?;
            }
        }
        Ok(())
    }

    pub fn memory(&self, slot: u32) -> Option<vk::DeviceMemory> {
        self.memories.get(slot as usize).copied()
    }

    pub fn uses_external_queue_family(&self) -> bool {
        matches!(
            self.image_kind,
            ExternalImageKind::D3d12Resource | ExternalImageKind::D3d11Resource
        )
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
            if let Some(semaphore) = self.consumer_timeline.take() {
                self.device.destroy_semaphore(semaphore, None);
            }
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
    Vulkan(vk::Result),
    NoCompatibleDevice,
    InvalidStream(&'static str),
}

impl fmt::Display for ImportError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vulkan(error) => write!(output, "Vulkan import failed: {error}"),
            Self::NoCompatibleDevice => write!(output, "no compatible Vulkan device was found"),
            Self::InvalidStream(message) => write!(output, "invalid stream: {message}"),
        }
    }
}

impl std::error::Error for ImportError {}
impl From<vk::Result> for ImportError {
    fn from(value: vk::Result) -> Self {
        Self::Vulkan(value)
    }
}

fn validate_consumer_semaphore(stream: &StreamReady) -> Result<(), ImportError> {
    if matches!(
        stream.image_kind,
        ExternalImageKind::D3d12Resource | ExternalImageKind::D3d11Resource
    ) && stream.consumer_semaphore_handle == 0
    {
        Err(ImportError::InvalidStream(
            "D3D12 consumer semaphore handle is missing",
        ))
    } else {
        Ok(())
    }
}

fn requested_adapter(stream: &StreamReady) -> Option<AdapterLuid> {
    (stream.adapter_luid.low_part != 0 || stream.adapter_luid.high_part != 0)
        .then_some(stream.adapter_luid)
}

pub fn physical_device_matches_luid(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    luid: AdapterLuid,
) -> bool {
    let mut id = vk::PhysicalDeviceIDProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
    unsafe { instance.get_physical_device_properties2(physical_device, &mut properties) };
    let expected = [luid.low_part.to_le_bytes(), luid.high_part.to_le_bytes()].concat();
    id.device_luid_valid == vk::TRUE && id.device_luid[..8] == expected
}

unsafe fn import_resources(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    stream: &StreamReady,
) -> Result<
    (
        Vec<vk::Image>,
        Vec<vk::DeviceMemory>,
        vk::Semaphore,
        Option<vk::Semaphore>,
    ),
    vk::Result,
> {
    let format = format_for_stream(stream).map_err(|_| vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?;
    let memory_handle_type = match stream.image_kind {
        ExternalImageKind::D3d11Resource => vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE,
        ExternalImageKind::D3d12Resource => vk::ExternalMemoryHandleTypeFlags::D3D12_RESOURCE,
        ExternalImageKind::IOSurface => return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED),
    };
    let semaphore_handle_type = match stream.sync_kind {
        ExternalSyncKind::D3d12Fence => vk::ExternalSemaphoreHandleTypeFlags::D3D12_FENCE,
        _ => return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT),
    };
    let memory_properties =
        unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let mut images = Vec::new();
    let mut memories = Vec::new();
    for raw_handle in stream.image_handles.iter().take(stream.slot_count as usize) {
        let mut external =
            vk::ExternalMemoryImageCreateInfo::default().handle_types(memory_handle_type);
        let mut usage = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED;
        if stream.hook == GraphicsHook::OpenGl {
            usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST;
        }
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
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { device.create_image(&image_info, None)? };
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let mut compatible_bits = requirements.memory_type_bits;
        if memory_handle_type != vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32 {
            let importer = ash::khr::external_memory_win32::Device::new(instance, device);
            let mut handle_properties = vk::MemoryWin32HandlePropertiesKHR::default();
            unsafe {
                importer.get_memory_win32_handle_properties(
                    memory_handle_type,
                    *raw_handle as isize,
                    &mut handle_properties,
                )?;
            }
            compatible_bits &= handle_properties.memory_type_bits;
        }
        let memory_type_index = memory_properties.memory_types
            [..memory_properties.memory_type_count as usize]
            .iter()
            .enumerate()
            .find(|(index, _)| compatible_bits & (1 << index) != 0)
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
        let allocation_result = unsafe { device.allocate_memory(&allocation, None) };
        close_win32_handle(*raw_handle);
        let memory = allocation_result?;
        unsafe { device.bind_image_memory(image, memory, 0)? };
        images.push(image);
        memories.push(memory);
    }
    let importer = ash::khr::external_semaphore_win32::Device::new(instance, device);
    let timeline = unsafe {
        import_timeline(
            device,
            &importer,
            semaphore_handle_type,
            stream.timeline_semaphore_handle,
        )?
    };
    let consumer_timeline = if stream.consumer_semaphore_handle != 0 {
        Some(unsafe {
            import_timeline(
                device,
                &importer,
                semaphore_handle_type,
                stream.consumer_semaphore_handle,
            )?
        })
    } else {
        None
    };
    Ok((images, memories, timeline, consumer_timeline))
}

unsafe fn import_timeline(
    device: &ash::Device,
    importer: &ash::khr::external_semaphore_win32::Device,
    handle_type: vk::ExternalSemaphoreHandleTypeFlags,
    handle: i64,
) -> Result<vk::Semaphore, vk::Result> {
    let mut timeline_type =
        vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
    let semaphore = unsafe {
        device.create_semaphore(
            &vk::SemaphoreCreateInfo::default().push_next(&mut timeline_type),
            None,
        )?
    };
    let import = vk::ImportSemaphoreWin32HandleInfoKHR::default()
        .semaphore(semaphore)
        .handle_type(handle_type)
        .handle(handle as isize);
    let import_result = unsafe { importer.import_semaphore_win32_handle(&import) };
    close_win32_handle(handle);
    if let Err(error) = import_result {
        unsafe { device.destroy_semaphore(semaphore, None) };
        return Err(error);
    }
    Ok(semaphore)
}

fn close_win32_handle(handle: i64) {
    if handle != 0 && handle != -1 {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle as _) };
    }
}
