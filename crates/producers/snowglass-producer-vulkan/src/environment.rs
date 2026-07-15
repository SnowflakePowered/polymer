use ash::{Entry, vk};
use snowglass_protocol::AdapterLuid;
use std::{error::Error, ffi::CStr, fmt};

pub const REQUIRED_DEVICE_EXTENSIONS: [&CStr; 4] = [
    ash::khr::swapchain::NAME,
    ash::khr::external_memory_win32::NAME,
    ash::khr::external_semaphore_win32::NAME,
    ash::khr::timeline_semaphore::NAME,
];

#[derive(Clone, Debug)]
pub struct VulkanEnvironment {
    pub loader_api_version: u32,
    pub instance_extensions: Vec<String>,
    pub instance_layers: Vec<String>,
    pub adapters: Vec<VulkanAdapter>,
}

#[derive(Clone, Debug)]
pub struct VulkanAdapter {
    pub name: String,
    pub api_version: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    pub adapter_luid: Option<AdapterLuid>,
    pub device_extensions: Vec<String>,
}

impl VulkanAdapter {
    pub fn supports_capture_transport(&self) -> bool {
        REQUIRED_DEVICE_EXTENSIONS.iter().all(|required| {
            self.device_extensions
                .iter()
                .any(|available| available.as_bytes() == required.to_bytes())
        })
    }
}

#[derive(Debug)]
pub enum VulkanProbeError {
    Loader(ash::LoadingError),
    Vulkan(vk::Result),
}

impl fmt::Display for VulkanProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader(error) => write!(formatter, "could not load Vulkan: {error}"),
            Self::Vulkan(error) => write!(formatter, "Vulkan call failed: {error}"),
        }
    }
}

impl Error for VulkanProbeError {}

impl From<ash::LoadingError> for VulkanProbeError {
    fn from(error: ash::LoadingError) -> Self {
        Self::Loader(error)
    }
}

impl From<vk::Result> for VulkanProbeError {
    fn from(error: vk::Result) -> Self {
        Self::Vulkan(error)
    }
}

impl VulkanEnvironment {
    /// Loads the system Vulkan loader and inventories capture prerequisites.
    pub fn probe() -> Result<Self, VulkanProbeError> {
        // SAFETY: `Entry` owns the loaded Vulkan module for the duration of all
        // calls in this function, and every Vulkan input uses initialized ash
        // structures with no borrowed pointers surviving the call.
        unsafe { Self::probe_unsafe() }
    }

    unsafe fn probe_unsafe() -> Result<Self, VulkanProbeError> {
        let entry = unsafe { Entry::load()? };
        let loader_api_version =
            unsafe { entry.try_enumerate_instance_version()? }.unwrap_or(vk::API_VERSION_1_0);
        let instance_extensions = unsafe { entry.enumerate_instance_extension_properties(None)? }
            .iter()
            .map(|property| fixed_c_string(&property.extension_name))
            .collect();
        let instance_layers = unsafe { entry.enumerate_instance_layer_properties()? }
            .iter()
            .map(|property| fixed_c_string(&property.layer_name))
            .collect();

        let application = vk::ApplicationInfo::default()
            .application_name(c"snowglass-probe")
            .application_version(1)
            .engine_name(c"snowglass")
            .engine_version(1)
            .api_version(loader_api_version.min(vk::API_VERSION_1_3));
        let create_info = vk::InstanceCreateInfo::default().application_info(&application);
        let instance = unsafe { entry.create_instance(&create_info, None)? };

        let adapter_result = (|| {
            let physical_devices = unsafe { instance.enumerate_physical_devices()? };
            physical_devices
                .into_iter()
                .map(|physical_device| {
                    let properties =
                        unsafe { instance.get_physical_device_properties(physical_device) };
                    let mut id_properties = vk::PhysicalDeviceIDProperties::default();
                    let mut properties2 =
                        vk::PhysicalDeviceProperties2::default().push_next(&mut id_properties);
                    unsafe {
                        instance.get_physical_device_properties2(physical_device, &mut properties2)
                    };
                    let device_extensions =
                        unsafe { instance.enumerate_device_extension_properties(physical_device)? }
                            .iter()
                            .map(|property| fixed_c_string(&property.extension_name))
                            .collect();
                    let adapter_luid = (id_properties.device_luid_valid == vk::TRUE).then(|| {
                        let bytes = id_properties.device_luid;
                        AdapterLuid {
                            low_part: u32::from_le_bytes(
                                bytes[0..4].try_into().expect("LUID size"),
                            ),
                            high_part: i32::from_le_bytes(
                                bytes[4..8].try_into().expect("LUID size"),
                            ),
                        }
                    });

                    Ok(VulkanAdapter {
                        name: fixed_c_string(&properties.device_name),
                        api_version: properties.api_version,
                        vendor_id: properties.vendor_id,
                        device_id: properties.device_id,
                        adapter_luid,
                        device_extensions,
                    })
                })
                .collect::<Result<Vec<_>, VulkanProbeError>>()
        })();

        unsafe { instance.destroy_instance(None) };

        Ok(Self {
            loader_api_version,
            instance_extensions,
            instance_layers,
            adapters: adapter_result?,
        })
    }
}

fn fixed_c_string(value: &[std::ffi::c_char]) -> String {
    // SAFETY: Vulkan fixed-size name fields are null-terminated strings.
    unsafe { CStr::from_ptr(value.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

pub fn format_api_version(version: u32) -> String {
    format!(
        "{}.{}.{}",
        vk::api_version_major(version),
        vk::api_version_minor(version),
        vk::api_version_patch(version)
    )
}
