use polymer_protocol::GraphicsHook;
use std::sync::OnceLock;
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::{D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice},
        Direct3D12::{D3D12CreateDevice, ID3D12Device},
    },
};

/// Graphics APIs this machine runs and Polymer captures, in preference order.
pub struct GraphicsSupport;

impl GraphicsSupport {
    pub fn available() -> &'static [GraphicsHook] {
        static AVAILABLE: OnceLock<Vec<GraphicsHook>> = OnceLock::new();
        AVAILABLE.get_or_init(|| {
            [
                GraphicsHook::Vulkan,
                GraphicsHook::D3d12,
                GraphicsHook::D3d11,
                GraphicsHook::OpenGl,
            ]
            .into_iter()
            .filter(|hook| Self::probe(*hook))
            .collect()
        })
    }

    fn probe(hook: GraphicsHook) -> bool {
        match hook {
            GraphicsHook::Vulkan => Self::vulkan(),
            GraphicsHook::D3d12 => unsafe {
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut None::<ID3D12Device>)
            }
            .is_ok(),
            GraphicsHook::D3d11 => unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_FLAG(0),
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    None,
                    None,
                    None,
                )
            }
            .is_ok(),
            GraphicsHook::OpenGl => true,
            GraphicsHook::Metal => false,
        }
    }

    fn vulkan() -> bool {
        let Ok(entry) = (unsafe { ash::Entry::load() }) else {
            return false;
        };
        let application = ash::vk::ApplicationInfo::default().api_version(ash::vk::API_VERSION_1_1);
        let info = ash::vk::InstanceCreateInfo::default().application_info(&application);
        let Ok(instance) = (unsafe { entry.create_instance(&info, None) }) else {
            return false;
        };
        let found = unsafe { instance.enumerate_physical_devices() }
            .is_ok_and(|devices| !devices.is_empty());
        unsafe { instance.destroy_instance(None) };
        found
    }
}
