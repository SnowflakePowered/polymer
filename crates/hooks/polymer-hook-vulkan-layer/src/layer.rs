//! Vulkan loader integration and shared dispatch state.
#![allow(non_snake_case)]

use ash::{vk, vk::Handle};
use dashmap::DashMap;
use polymer_hook::HookChain;
use polymer_producer_vulkan::SetDeviceLoaderData;
use std::{
    ffi::{c_char, c_void},
    mem,
    sync::{Arc, OnceLock},
};

pub(crate) use polymer_producer_vulkan::ExternalDevice;

const VK_LAYER_LINK_INFO: u32 = 0;

pub(crate) type PfnGetPhysicalDeviceProcAddr =
    unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction;

#[repr(C)]
pub struct VkNegotiateLayerInterface {
    pub(crate) s_type: u32,
    pub(crate) p_next: *mut c_void,
    pub(crate) loader_layer_interface_version: u32,
    pub(crate) pfn_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pub(crate) pfn_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
    pub(crate) pfn_get_physical_device_proc_addr: Option<PfnGetPhysicalDeviceProcAddr>,
}

#[repr(C)]
struct VkLayerInstanceLink {
    p_next: *mut Self,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_physical_device_proc_addr: Option<PfnGetPhysicalDeviceProcAddr>,
}

#[repr(C)]
union VkLayerInstanceCreateInfoData {
    p_layer_info: *mut VkLayerInstanceLink,
    opaque: *mut c_void,
}

#[repr(C)]
struct VkLayerInstanceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32,
    data: VkLayerInstanceCreateInfoData,
}

#[repr(C)]
struct VkLayerDeviceLink {
    p_next: *mut Self,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
union VkLayerDeviceCreateInfoData {
    p_layer_info: *mut VkLayerDeviceLink,
    opaque: *mut c_void,
}

#[repr(C)]
struct VkLayerDeviceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32,
    data: VkLayerDeviceCreateInfoData,
}

#[derive(Clone)]
pub(crate) struct InstanceDispatchTable {
    pub(crate) next_gipa: vk::PFN_vkGetInstanceProcAddr,
    pub(crate) next_physical_gpa: Option<PfnGetPhysicalDeviceProcAddr>,
}

#[derive(Clone)]
pub(crate) struct DeviceDispatchTable {
    pub next_gdpa: vk::PFN_vkGetDeviceProcAddr,
    pub physical_device: vk::PhysicalDevice,
    pub physical_state: Option<PhysicalDeviceState>,
    pub external: Option<Arc<ExternalDevice>>,
    pub handle: vk::Device,
}

#[derive(Clone)]
pub(crate) struct QueueDispatchTable {
    pub(crate) device: vk::Device,
    pub(crate) dispatch: DeviceDispatchTable,
}

#[derive(Clone)]
pub(crate) struct PhysicalDeviceState {
    pub instance: vk::Instance,
    pub next_gipa: vk::PFN_vkGetInstanceProcAddr,
}

#[derive(Default)]
pub(crate) struct VulkanHookState {
    pub(crate) device: DashMap<usize, DeviceDispatchTable>,
    pub(crate) instance: DashMap<usize, InstanceDispatchTable>,
    pub(crate) physical_device: DashMap<vk::PhysicalDevice, PhysicalDeviceState>,
    pub(crate) queue: DashMap<vk::Queue, QueueDispatchTable>,
}

static HOOK_STATE: OnceLock<Arc<VulkanHookState>> = OnceLock::new();

pub(crate) fn install_hook_state(state: Arc<VulkanHookState>) -> Arc<VulkanHookState> {
    let _ = HOOK_STATE.set(state);
    Arc::clone(HOOK_STATE.get().expect("Vulkan hook state was installed"))
}

pub(crate) fn hook_state() -> Arc<VulkanHookState> {
    Arc::clone(HOOK_STATE.get_or_init(|| Arc::new(VulkanHookState::default())))
}

pub(crate) unsafe fn dispatch_key<H: Handle>(handle: H) -> Option<usize> {
    let raw = handle.as_raw() as usize;
    if raw == 0 {
        return None;
    }
    // All dispatchable handles begin with the loader's dispatch-table pointer.
    Some(unsafe { *(raw as *const usize) })
}

unsafe fn find_instance_link(
    create_info: *const vk::InstanceCreateInfo<'_>,
) -> Option<*mut VkLayerInstanceCreateInfo> {
    if create_info.is_null() {
        return None;
    }
    let mut current = unsafe { (*create_info).p_next };
    while !current.is_null() {
        let base = current.cast::<vk::BaseInStructure<'_>>();
        if unsafe { (*base).s_type } == vk::StructureType::LOADER_INSTANCE_CREATE_INFO {
            let layer_info = current.cast_mut().cast::<VkLayerInstanceCreateInfo>();
            if unsafe { (*layer_info).function } == VK_LAYER_LINK_INFO {
                return Some(layer_info);
            }
        }
        current = unsafe { (*base).p_next }.cast();
    }
    None
}

unsafe fn find_device_link(
    create_info: *const vk::DeviceCreateInfo<'_>,
) -> Option<*mut VkLayerDeviceCreateInfo> {
    if create_info.is_null() {
        return None;
    }
    let mut current = unsafe { (*create_info).p_next };
    while !current.is_null() {
        let base = current.cast::<vk::BaseInStructure<'_>>();
        if unsafe { (*base).s_type } == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
            let layer_info = current.cast_mut().cast::<VkLayerDeviceCreateInfo>();
            if unsafe { (*layer_info).function } == VK_LAYER_LINK_INFO {
                return Some(layer_info);
            }
        }
        current = unsafe { (*base).p_next }.cast();
    }
    None
}

unsafe fn find_device_loader_data_callback(
    create_info: *const vk::DeviceCreateInfo<'_>,
) -> Option<SetDeviceLoaderData> {
    if create_info.is_null() {
        return None;
    }
    let mut current = unsafe { (*create_info).p_next };
    while !current.is_null() {
        let base = current.cast::<vk::BaseInStructure<'_>>();
        if unsafe { (*base).s_type } == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
            let layer_info = current.cast::<VkLayerDeviceCreateInfo>();
            if unsafe { (*layer_info).function } == 1 {
                let callback = unsafe { (*layer_info).data.opaque };
                if !callback.is_null() {
                    return Some(unsafe {
                        mem::transmute::<*mut std::ffi::c_void, SetDeviceLoaderData>(callback)
                    });
                }
            }
        }
        current = unsafe { (*base).p_next }.cast();
    }
    None
}

pub(crate) struct InstanceBootstrap {
    pub next_gipa: vk::PFN_vkGetInstanceProcAddr,
    pub next_physical_gpa: Option<PfnGetPhysicalDeviceProcAddr>,
}

pub(crate) unsafe fn take_instance_bootstrap(
    create_info: *const vk::InstanceCreateInfo<'_>,
) -> Option<InstanceBootstrap> {
    let Some(chain_info) = (unsafe { find_instance_link(create_info) }) else {
        tracing::debug!("polymer: loader instance link was not present");
        return None;
    };
    let link = unsafe { (*chain_info).data.p_layer_info };
    if link.is_null() {
        return None;
    }
    let next_gipa = unsafe { (*link).pfn_next_get_instance_proc_addr };
    let next_physical_gpa = unsafe { (*link).pfn_next_get_physical_device_proc_addr };
    unsafe { (*chain_info).data.p_layer_info = (*link).p_next };
    Some(InstanceBootstrap {
        next_gipa,
        next_physical_gpa,
    })
}

pub(crate) fn register_instance_dispatch(created: vk::Instance, bootstrap: InstanceBootstrap) {
    if let Some(key) = unsafe { dispatch_key(created) } {
        {
            hook_state().instance.insert(
                key,
                InstanceDispatchTable {
                    next_gipa: bootstrap.next_gipa,
                    next_physical_gpa: bootstrap.next_physical_gpa,
                },
            );
        }
    }
}

pub(crate) unsafe fn handle_destroy_instance(
    instance: vk::Instance,
    allocator: *const vk::AllocationCallbacks<'_>,
    mut next: crate::hooks::DestroyInstanceContext<'_>,
) {
    if crate::swapchain::Device::retains_instance(instance) {
        return;
    }
    let continuation = *next
        .fp_next()
        .expect("Vulkan around chains always have a terminal");
    hook_state()
        .physical_device
        .retain(|_, state| state.instance != instance);
    let dispatch = unsafe { dispatch_key(instance) }.and_then(|key| {
        hook_state()
            .instance
            .remove(&key)
            .map(|(_, dispatch)| dispatch)
    });
    let _ = dispatch;
    unsafe { continuation(instance, allocator, next) };
}

pub(crate) struct DeviceBootstrap {
    pub next_gipa: vk::PFN_vkGetInstanceProcAddr,
    pub next_gdpa: vk::PFN_vkGetDeviceProcAddr,
    pub set_loader_data: Option<SetDeviceLoaderData>,
}

pub(crate) unsafe fn take_device_bootstrap(
    create_info: *const vk::DeviceCreateInfo<'_>,
) -> Option<DeviceBootstrap> {
    let set_loader_data = unsafe { find_device_loader_data_callback(create_info) };
    let chain_info = unsafe { find_device_link(create_info) }?;
    let link = unsafe { (*chain_info).data.p_layer_info };
    if link.is_null() {
        return None;
    }
    let bootstrap = DeviceBootstrap {
        next_gipa: unsafe { (*link).pfn_next_get_instance_proc_addr },
        next_gdpa: unsafe { (*link).pfn_next_get_device_proc_addr },
        set_loader_data,
    };
    unsafe { (*chain_info).data.p_layer_info = (*link).p_next };
    Some(bootstrap)
}

pub(crate) fn physical_device_state(device: vk::PhysicalDevice) -> Option<PhysicalDeviceState> {
    hook_state()
        .physical_device
        .get(&device)
        .map(|entry| entry.clone())
}

pub(crate) fn register_device_dispatch(device: vk::Device, dispatch: DeviceDispatchTable) {
    if let Some(key) = unsafe { dispatch_key(device) } {
        {
            hook_state().device.insert(key, dispatch);
        }
    }
}

pub(crate) fn observe_create_device(injected_extension_count: usize) {
    if injected_extension_count != 0 {
        tracing::debug!("polymer: enabled {injected_extension_count} swapchain device extensions");
    }
}

pub(crate) unsafe fn handle_destroy_device(
    device: vk::Device,
    allocator: *const vk::AllocationCallbacks<'_>,
    mut next: crate::hooks::DestroyDeviceContext<'_>,
) {
    if !crate::swapchain::Device::destroy(device) {
        return;
    }
    let continuation = *next
        .fp_next()
        .expect("Vulkan around chains always have a terminal");
    let _dispatch = unsafe { dispatch_key(device) }.and_then(|key| {
        hook_state()
            .device
            .remove(&key)
            .map(|(_, dispatch)| dispatch)
    });
    hook_state()
        .queue
        .retain(|_, dispatch| dispatch.device != device);
    unsafe { continuation(device, allocator, next) };
}

pub(crate) fn command_dispatch_data(
    command_buffer: vk::CommandBuffer,
) -> Option<(vk::PFN_vkGetDeviceProcAddr, vk::Device)> {
    let state = hook_state();
    let dispatch = unsafe { dispatch_key(command_buffer) }
        .and_then(|key| state.device.get(&key).map(|entry| entry.clone()))?;
    let device = dispatch.handle;
    Some((dispatch.next_gdpa, device))
}

pub(crate) fn instance_gipa(instance: vk::Instance) -> Option<vk::PFN_vkGetInstanceProcAddr> {
    unsafe { dispatch_key(instance) }
        .and_then(|key| hook_state().instance.get(&key).map(|entry| entry.next_gipa))
}

pub(crate) fn instance_physical_gpa(
    instance: vk::Instance,
) -> Option<PfnGetPhysicalDeviceProcAddr> {
    unsafe { dispatch_key(instance) }.and_then(|key| {
        hook_state()
            .instance
            .get(&key)
            .and_then(|entry| entry.next_physical_gpa)
    })
}

pub(crate) fn device_gdpa(device: vk::Device) -> Option<vk::PFN_vkGetDeviceProcAddr> {
    unsafe { dispatch_key(device) }
        .and_then(|key| hook_state().device.get(&key).map(|entry| entry.next_gdpa))
}

pub(crate) fn queue_dispatch_data(
    queue: vk::Queue,
) -> Option<(vk::PFN_vkGetDeviceProcAddr, vk::Device)> {
    let state = hook_state();
    let state = state.queue.get(&queue)?;
    Some((state.dispatch.next_gdpa, state.device))
}
