//! Vulkan loader layer that transparently observes swapchain presentation.
#![allow(static_mut_refs)]

use ash::{vk, vk::Handle};
use dashmap::DashMap;
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_producer_vulkan::{
    ExportImageRing, ExportImageRingDesc, ExportTimelineSemaphore, SetDeviceLoaderData,
    SwapchainCopyEngine,
};
use snowglass_protocol::{FrameReady, MAX_IMAGE_SLOTS, PROTOCOL_VERSION, StreamId};
use std::{
    cell::LazyCell,
    ffi::{CStr, c_char, c_void},
    fs::OpenOptions,
    io::Write,
    mem, ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

fn layer_log(message: impl std::fmt::Display) {
    eprintln!("{message}");
    let Ok(path) = std::env::var("SNOWGLASS_LOG_FILE") else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

fn connect_viewer(
    ring: &ExportImageRing,
    timeline: &ExportTimelineSemaphore,
    extent: vk::Extent2D,
    format: vk::Format,
) -> Option<LocalSocket> {
    let session = std::env::var("SNOWGLASS_SESSION").ok()?;
    let mut socket = match LocalSocket::connect(&session) {
        Ok(socket) => socket,
        Err(error) => {
            layer_log(format!(
                "snowglass: viewer socket connection failed: {error}"
            ));
            return None;
        }
    };
    let viewer_pid = match socket.receive() {
        Ok(Message::Hello(hello)) if hello.protocol_version == PROTOCOL_VERSION => hello.process_id,
        Ok(Message::Hello(hello)) => {
            layer_log(format!(
                "snowglass: viewer protocol {} is incompatible with {}",
                hello.protocol_version, PROTOCOL_VERSION
            ));
            return None;
        }
        Ok(_) => {
            layer_log("snowglass: viewer did not begin with a Hello message");
            return None;
        }
        Err(error) => {
            layer_log(format!("snowglass: viewer handshake failed: {error}"));
            return None;
        }
    };
    if ring.slots().len() > MAX_IMAGE_SLOTS {
        layer_log("snowglass: export ring exceeds protocol slot capacity");
        return None;
    }
    let mut handles = [0; MAX_IMAGE_SLOTS];
    for (destination, slot) in handles.iter_mut().zip(ring.slots()) {
        match duplicate_handle_into_process(slot.raw_handle(), ProcessId(viewer_pid)) {
            Ok(handle) => *destination = handle as i64,
            Err(error) => {
                layer_log(format!(
                    "snowglass: image handle duplication failed: {error}"
                ));
                return None;
            }
        }
    }
    let ready = StreamReady {
        producer_process_id: std::process::id(),
        width: extent.width,
        height: extent.height,
        format: format.as_raw() as u32,
        slot_count: ring.slots().len() as u32,
        image_handles: handles,
        timeline_semaphore_handle: match duplicate_handle_into_process(
            timeline.raw_handle(),
            ProcessId(viewer_pid),
        ) {
            Ok(handle) => handle as i64,
            Err(error) => {
                layer_log(format!(
                    "snowglass: timeline handle duplication failed: {error}"
                ));
                return None;
            }
        },
    };
    if let Err(error) = socket.send(Message::StreamReady(ready)) {
        layer_log(format!("snowglass: StreamReady send failed: {error}"));
        return None;
    }
    layer_log(format!(
        "snowglass: connected export ring to viewer process {viewer_pid}"
    ));
    Some(socket)
}

const LOADER_LAYER_INTERFACE_VERSION: u32 = 2;
const VK_LAYER_LINK_INFO: u32 = 0;

type PfnGetPhysicalDeviceProcAddr =
    unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction;

#[repr(C)]
pub struct VkNegotiateLayerInterface {
    s_type: u32,
    p_next: *mut c_void,
    loader_layer_interface_version: u32,
    pfn_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pfn_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
    pfn_get_physical_device_proc_addr: Option<PfnGetPhysicalDeviceProcAddr>,
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

#[derive(Clone, Copy)]
struct InstanceDispatchTable {
    next_gipa: vk::PFN_vkGetInstanceProcAddr,
    next_physical_gpa: Option<PfnGetPhysicalDeviceProcAddr>,
    destroy_instance: vk::PFN_vkDestroyInstance,
}

#[derive(Clone)]
struct DeviceDispatchTable {
    next_gdpa: vk::PFN_vkGetDeviceProcAddr,
    destroy_device: vk::PFN_vkDestroyDevice,
    create_swapchain: Option<vk::PFN_vkCreateSwapchainKHR>,
    destroy_swapchain: Option<vk::PFN_vkDestroySwapchainKHR>,
    get_swapchain_images: Option<vk::PFN_vkGetSwapchainImagesKHR>,
    queue_present: Option<vk::PFN_vkQueuePresentKHR>,
    capture: Option<Arc<CaptureDevice>>,
}

struct SwapchainState {
    device: vk::Device,
    format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    ring: Option<ExportImageRing>,
    timeline: Option<ExportTimelineSemaphore>,
    copy_engine: Option<SwapchainCopyEngine>,
    ipc: Option<LocalSocket>,
    frame_number: u64,
}

#[derive(Clone)]
struct QueueDispatchTable {
    device: vk::Device,
    queue_family_index: u32,
    dispatch: DeviceDispatchTable,
}

struct CaptureDevice {
    device: ash::Device,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    get_memory_win32_handle: vk::PFN_vkGetMemoryWin32HandleKHR,
    get_semaphore_win32_handle: vk::PFN_vkGetSemaphoreWin32HandleKHR,
    set_loader_data: Option<SetDeviceLoaderData>,
}

#[derive(Clone, Copy)]
struct PhysicalDeviceState {
    instance: vk::Instance,
    next_gipa: vk::PFN_vkGetInstanceProcAddr,
}

#[allow(clippy::type_complexity)]
static mut DEVICE: LazyCell<DashMap<usize, DeviceDispatchTable>> = LazyCell::new(Default::default);

#[allow(clippy::type_complexity)]
static mut INSTANCE: LazyCell<DashMap<usize, InstanceDispatchTable>> =
    LazyCell::new(Default::default);

static mut PHYSICAL_DEVICE: LazyCell<DashMap<vk::PhysicalDevice, PhysicalDeviceState>> =
    LazyCell::new(Default::default);

#[allow(clippy::type_complexity)]
static mut QUEUE: LazyCell<DashMap<vk::Queue, QueueDispatchTable>> =
    LazyCell::new(Default::default);

static mut SWAPCHAIN: LazyCell<DashMap<vk::SwapchainKHR, SwapchainState>> =
    LazyCell::new(Default::default);

static SWAPCHAINS_CREATED: AtomicU64 = AtomicU64::new(0);
static PRESENTS_SEEN: AtomicU64 = AtomicU64::new(0);
static COPIES_SUBMITTED: AtomicU64 = AtomicU64::new(0);

unsafe fn dispatch_key<H: Handle>(handle: H) -> Option<usize> {
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
                    return Some(unsafe { mem::transmute(callback) });
                }
            }
        }
        current = unsafe { (*base).p_next }.cast();
    }
    None
}

unsafe fn instance_proc<T: Copy>(
    gipa: vk::PFN_vkGetInstanceProcAddr,
    instance: vk::Instance,
    name: &CStr,
) -> Option<T> {
    let function = unsafe { gipa(instance, name.as_ptr()) }?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&function));
    let pointer = &function as *const unsafe extern "system" fn();
    Some(unsafe { ptr::read(pointer.cast::<T>()) })
}

unsafe fn device_proc<T: Copy>(
    gdpa: vk::PFN_vkGetDeviceProcAddr,
    device: vk::Device,
    name: &CStr,
) -> Option<T> {
    let function = unsafe { gdpa(device, name.as_ptr()) }?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&function));
    let pointer = &function as *const unsafe extern "system" fn();
    Some(unsafe { ptr::read(pointer.cast::<T>()) })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    interface: *mut VkNegotiateLayerInterface,
) -> vk::Result {
    if interface.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let requested = unsafe { (*interface).loader_layer_interface_version };
    if requested == 0 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    unsafe {
        (*interface).loader_layer_interface_version = requested.min(LOADER_LAYER_INTERFACE_VERSION);
        (*interface).pfn_get_instance_proc_addr = Some(vkGetInstanceProcAddr);
        (*interface).pfn_get_device_proc_addr = Some(vkGetDeviceProcAddr);
        (*interface).pfn_get_physical_device_proc_addr = Some(vk_layerGetPhysicalDeviceProcAddr);
    }
    vk::Result::SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetInstanceProcAddr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) };
    match name.to_bytes() {
        b"vkGetInstanceProcAddr" => {
            return Some(unsafe { mem::transmute(vkGetInstanceProcAddr as *const ()) });
        }
        b"vkCreateInstance" => {
            return Some(unsafe { mem::transmute(vkCreateInstance as *const ()) });
        }
        b"vkDestroyInstance" => {
            return Some(unsafe { mem::transmute(vkDestroyInstance as *const ()) });
        }
        b"vkCreateDevice" => {
            return Some(unsafe { mem::transmute(vkCreateDevice as *const ()) });
        }
        b"vkEnumeratePhysicalDevices" => {
            return Some(unsafe { mem::transmute(vkEnumeratePhysicalDevices as *const ()) });
        }
        b"vkGetDeviceProcAddr" => {
            return Some(unsafe { mem::transmute(vkGetDeviceProcAddr as *const ()) });
        }
        _ => {}
    }
    let dispatch = unsafe { dispatch_key(instance) }
        .and_then(|key| unsafe { INSTANCE.get(&key).map(|entry| *entry) });
    dispatch.and_then(|entry| unsafe { (entry.next_gipa)(instance, name.as_ptr()) })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vk_layerGetPhysicalDeviceProcAddr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let dispatch = unsafe { dispatch_key(instance) }
        .and_then(|key| unsafe { INSTANCE.get(&key).map(|entry| *entry) });
    dispatch.and_then(|entry| unsafe {
        if let Some(next) = entry.next_physical_gpa {
            next(instance, name)
        } else {
            (entry.next_gipa)(instance, name)
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceProcAddr(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) };
    match name.to_bytes() {
        b"vkGetDeviceProcAddr" => {
            return Some(unsafe { mem::transmute(vkGetDeviceProcAddr as *const ()) });
        }
        b"vkDestroyDevice" => {
            return Some(unsafe { mem::transmute(vkDestroyDevice as *const ()) });
        }
        b"vkCreateSwapchainKHR" => {
            return Some(unsafe { mem::transmute(vkCreateSwapchainKHR as *const ()) });
        }
        b"vkDestroySwapchainKHR" => {
            return Some(unsafe { mem::transmute(vkDestroySwapchainKHR as *const ()) });
        }
        b"vkGetSwapchainImagesKHR" => {
            return Some(unsafe { mem::transmute(vkGetSwapchainImagesKHR as *const ()) });
        }
        b"vkQueuePresentKHR" => {
            return Some(unsafe { mem::transmute(vkQueuePresentKHR as *const ()) });
        }
        b"vkGetDeviceQueue" => {
            return Some(unsafe { mem::transmute(vkGetDeviceQueue as *const ()) });
        }
        b"vkGetDeviceQueue2" => {
            return Some(unsafe { mem::transmute(vkGetDeviceQueue2 as *const ()) });
        }
        _ => {}
    }
    let dispatch = unsafe { dispatch_key(device) }
        .and_then(|key| unsafe { DEVICE.get(&key).map(|entry| entry.clone()) });
    dispatch.and_then(|entry| unsafe { (entry.next_gdpa)(device, name.as_ptr()) })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateInstance(
    create_info: *const vk::InstanceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    instance: *mut vk::Instance,
) -> vk::Result {
    layer_log("snowglass: vkCreateInstance entered");
    let Some(chain_info) = (unsafe { find_instance_link(create_info) }) else {
        layer_log("snowglass: loader instance link was not present");
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let link = unsafe { (*chain_info).data.p_layer_info };
    if link.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let next_gipa = unsafe { (*link).pfn_next_get_instance_proc_addr };
    let next_physical_gpa = unsafe { (*link).pfn_next_get_physical_device_proc_addr };
    unsafe { (*chain_info).data.p_layer_info = (*link).p_next };
    let Some(next_create): Option<vk::PFN_vkCreateInstance> =
        (unsafe { instance_proc(next_gipa, vk::Instance::null(), c"vkCreateInstance") })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let result = unsafe { next_create(create_info, allocator, instance) };
    layer_log(format!(
        "snowglass: downstream vkCreateInstance returned {result}"
    ));
    if result != vk::Result::SUCCESS || instance.is_null() {
        return result;
    }
    let created = unsafe { *instance };
    let Some(destroy_instance) =
        (unsafe { instance_proc(next_gipa, created, c"vkDestroyInstance") })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    if let Some(key) = unsafe { dispatch_key(created) } {
        unsafe {
            INSTANCE.insert(
                key,
                InstanceDispatchTable {
                    next_gipa,
                    next_physical_gpa,
                    destroy_instance,
                },
            );
        }
    }
    result
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDestroyInstance(
    instance: vk::Instance,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    unsafe { PHYSICAL_DEVICE.retain(|_, state| state.instance != instance) };
    let dispatch = unsafe { dispatch_key(instance) }
        .and_then(|key| unsafe { INSTANCE.remove(&key).map(|(_, dispatch)| dispatch) });
    if let Some(dispatch) = dispatch {
        unsafe { (dispatch.destroy_instance)(instance, allocator) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkEnumeratePhysicalDevices(
    instance: vk::Instance,
    physical_device_count: *mut u32,
    physical_devices: *mut vk::PhysicalDevice,
) -> vk::Result {
    let Some(dispatch) = (unsafe { dispatch_key(instance) })
        .and_then(|key| unsafe { INSTANCE.get(&key).map(|entry| *entry) })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(function): Option<vk::PFN_vkEnumeratePhysicalDevices> =
        (unsafe { instance_proc(dispatch.next_gipa, instance, c"vkEnumeratePhysicalDevices") })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let result = unsafe { function(instance, physical_device_count, physical_devices) };
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !physical_device_count.is_null()
        && !physical_devices.is_null()
    {
        for physical_device in
            unsafe { std::slice::from_raw_parts(physical_devices, *physical_device_count as usize) }
        {
            unsafe {
                PHYSICAL_DEVICE.insert(
                    *physical_device,
                    PhysicalDeviceState {
                        instance,
                        next_gipa: dispatch.next_gipa,
                    },
                );
            }
        }
    }
    result
}

unsafe fn device_supports_extension(
    state: PhysicalDeviceState,
    physical_device: vk::PhysicalDevice,
    required: &CStr,
) -> bool {
    let Some(enumerate): Option<vk::PFN_vkEnumerateDeviceExtensionProperties> = (unsafe {
        instance_proc(
            state.next_gipa,
            state.instance,
            c"vkEnumerateDeviceExtensionProperties",
        )
    }) else {
        return false;
    };
    let mut count = 0;
    if unsafe { enumerate(physical_device, ptr::null(), &mut count, ptr::null_mut()) }
        != vk::Result::SUCCESS
    {
        return false;
    }
    let mut properties = vec![vk::ExtensionProperties::default(); count as usize];
    let result = unsafe {
        enumerate(
            physical_device,
            ptr::null(),
            &mut count,
            properties.as_mut_ptr(),
        )
    };
    matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && properties[..count as usize].iter().any(|property| {
            (unsafe { CStr::from_ptr(property.extension_name.as_ptr()) }) == required
        })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateDevice(
    physical_device: vk::PhysicalDevice,
    create_info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    let set_loader_data = unsafe { find_device_loader_data_callback(create_info) };
    let Some(chain_info) = (unsafe { find_device_link(create_info) }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let link = unsafe { (*chain_info).data.p_layer_info };
    if link.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let next_gipa = unsafe { (*link).pfn_next_get_instance_proc_addr };
    let next_gdpa = unsafe { (*link).pfn_next_get_device_proc_addr };
    unsafe { (*chain_info).data.p_layer_info = (*link).p_next };
    let Some(next_create): Option<vk::PFN_vkCreateDevice> =
        (unsafe { instance_proc(next_gipa, vk::Instance::null(), c"vkCreateDevice") })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let physical_state = unsafe { PHYSICAL_DEVICE.get(&physical_device).map(|entry| *entry) };
    let mut extension_names =
        if create_info.is_null() || unsafe { (*create_info).enabled_extension_count } == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    (*create_info).pp_enabled_extension_names,
                    (*create_info).enabled_extension_count as usize,
                )
                .to_vec()
            }
        };
    let capture_extensions = [
        ash::khr::external_memory_win32::NAME,
        ash::khr::external_semaphore_win32::NAME,
        ash::khr::timeline_semaphore::NAME,
    ];
    let mut injected_extensions = Vec::new();
    for required in capture_extensions {
        let enabled = extension_names
            .iter()
            .any(|name| !name.is_null() && unsafe { CStr::from_ptr(*name) } == required);
        if !enabled
            && physical_state.is_some_and(|state| unsafe {
                device_supports_extension(state, physical_device, required)
            })
        {
            extension_names.push(required.as_ptr());
            injected_extensions.push(required);
        }
    }
    let capture_supported = capture_extensions.iter().all(|required| {
        extension_names
            .iter()
            .any(|name| !name.is_null() && unsafe { CStr::from_ptr(*name) } == *required)
    });
    let mut modified_create_info = (!create_info.is_null()).then(|| unsafe { *create_info });
    let mut timeline_features = vk::PhysicalDeviceTimelineSemaphoreFeatures::default()
        .timeline_semaphore(capture_supported);
    if let Some(info) = modified_create_info.as_mut() {
        if !injected_extensions.is_empty() {
            info.enabled_extension_count = extension_names.len() as u32;
            info.pp_enabled_extension_names = extension_names.as_ptr();
        }
        if capture_supported {
            timeline_features.p_next = info.p_next as *mut c_void;
            info.p_next = (&timeline_features as *const _) as *const c_void;
        }
    }
    let forwarded_create_info = modified_create_info
        .as_ref()
        .map_or(create_info, |info| info as *const _);
    let result = unsafe { next_create(physical_device, forwarded_create_info, allocator, device) };
    if result != vk::Result::SUCCESS || device.is_null() {
        return result;
    }
    let created = unsafe { *device };
    let Some(destroy_device) = (unsafe { device_proc(next_gdpa, created, c"vkDestroyDevice") })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let create_swapchain = unsafe { device_proc(next_gdpa, created, c"vkCreateSwapchainKHR") };
    let destroy_swapchain = unsafe { device_proc(next_gdpa, created, c"vkDestroySwapchainKHR") };
    let get_swapchain_images =
        unsafe { device_proc(next_gdpa, created, c"vkGetSwapchainImagesKHR") };
    let queue_present = unsafe { device_proc(next_gdpa, created, c"vkQueuePresentKHR") };
    let capture = physical_state.and_then(|state| {
        if !capture_supported {
            return None;
        }
        let get_memory_win32_handle =
            unsafe { device_proc(next_gdpa, created, c"vkGetMemoryWin32HandleKHR") }?;
        let get_semaphore_win32_handle =
            unsafe { device_proc(next_gdpa, created, c"vkGetSemaphoreWin32HandleKHR") }?;
        let get_memory_properties: vk::PFN_vkGetPhysicalDeviceMemoryProperties = unsafe {
            instance_proc(
                state.next_gipa,
                state.instance,
                c"vkGetPhysicalDeviceMemoryProperties",
            )
        }?;
        let mut memory_properties = vk::PhysicalDeviceMemoryProperties::default();
        unsafe { get_memory_properties(physical_device, &mut memory_properties) };
        let ash_device = unsafe {
            ash::Device::load_with(
                |name| {
                    next_gdpa(created, name.as_ptr()).map_or(ptr::null(), |function| {
                        function as *const () as *const c_void
                    })
                },
                created,
            )
        };
        Some(Arc::new(CaptureDevice {
            device: ash_device,
            memory_properties,
            get_memory_win32_handle,
            get_semaphore_win32_handle,
            set_loader_data,
        }))
    });
    if !injected_extensions.is_empty() {
        layer_log(format!(
            "snowglass: enabled {} capture device extensions",
            injected_extensions.len()
        ));
    }
    if let Some(key) = unsafe { dispatch_key(created) } {
        unsafe {
            DEVICE.insert(
                key,
                DeviceDispatchTable {
                    next_gdpa,
                    destroy_device,
                    create_swapchain,
                    destroy_swapchain,
                    get_swapchain_images,
                    queue_present,
                    capture,
                },
            );
        }
    }
    result
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDestroyDevice(
    device: vk::Device,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let dispatch = unsafe { dispatch_key(device) }
        .and_then(|key| unsafe { DEVICE.remove(&key).map(|(_, dispatch)| dispatch) });
    if let Some(capture) = dispatch.as_ref().and_then(|entry| entry.capture.as_ref()) {
        let _ = unsafe { capture.device.device_wait_idle() };
    }
    unsafe { QUEUE.retain(|_, dispatch| dispatch.device != device) };
    unsafe { SWAPCHAIN.retain(|_, state| state.device != device) };
    if let Some(dispatch) = dispatch {
        unsafe { (dispatch.destroy_device)(device, allocator) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceQueue(
    device: vk::Device,
    queue_family_index: u32,
    queue_index: u32,
    queue: *mut vk::Queue,
) {
    let Some(dispatch) = (unsafe { dispatch_key(device) })
        .and_then(|key| unsafe { DEVICE.get(&key).map(|entry| entry.clone()) })
    else {
        return;
    };
    let Some(function): Option<vk::PFN_vkGetDeviceQueue> =
        (unsafe { device_proc(dispatch.next_gdpa, device, c"vkGetDeviceQueue") })
    else {
        return;
    };
    unsafe { function(device, queue_family_index, queue_index, queue) };
    if !queue.is_null() {
        let queue = unsafe { *queue };
        if queue != vk::Queue::default() {
            unsafe {
                QUEUE.insert(
                    queue,
                    QueueDispatchTable {
                        device,
                        queue_family_index,
                        dispatch,
                    },
                )
            };
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceQueue2(
    device: vk::Device,
    queue_info: *const vk::DeviceQueueInfo2<'_>,
    queue: *mut vk::Queue,
) {
    let Some(dispatch) = (unsafe { dispatch_key(device) })
        .and_then(|key| unsafe { DEVICE.get(&key).map(|entry| entry.clone()) })
    else {
        return;
    };
    let Some(function): Option<vk::PFN_vkGetDeviceQueue2> =
        (unsafe { device_proc(dispatch.next_gdpa, device, c"vkGetDeviceQueue2") })
    else {
        return;
    };
    unsafe { function(device, queue_info, queue) };
    if !queue.is_null() {
        let queue = unsafe { *queue };
        if queue != vk::Queue::default() {
            let queue_family_index = if queue_info.is_null() {
                0
            } else {
                unsafe { (*queue_info).queue_family_index }
            };
            unsafe {
                QUEUE.insert(
                    queue,
                    QueueDispatchTable {
                        device,
                        queue_family_index,
                        dispatch,
                    },
                )
            };
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateSwapchainKHR(
    device: vk::Device,
    create_info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    swapchain: *mut vk::SwapchainKHR,
) -> vk::Result {
    let dispatch = unsafe { dispatch_key(device) }
        .and_then(|key| unsafe { DEVICE.get(&key).map(|entry| entry.clone()) });
    let mut modified_create_info = (!create_info.is_null()).then(|| unsafe { *create_info });
    if let Some(info) = modified_create_info.as_mut() {
        if dispatch
            .as_ref()
            .is_some_and(|entry| entry.capture.is_some())
        {
            info.image_usage |= vk::ImageUsageFlags::TRANSFER_SRC;
        }
    }
    let forwarded_create_info = modified_create_info
        .as_ref()
        .map_or(create_info, |info| info as *const _);
    let result = dispatch
        .as_ref()
        .and_then(|entry| entry.create_swapchain)
        .map_or(vk::Result::ERROR_EXTENSION_NOT_PRESENT, |function| unsafe {
            function(device, forwarded_create_info, allocator, swapchain)
        });
    if result == vk::Result::SUCCESS {
        let count = SWAPCHAINS_CREATED.fetch_add(1, Ordering::Relaxed) + 1;
        if !create_info.is_null() && !swapchain.is_null() {
            let info = modified_create_info
                .as_ref()
                .unwrap_or_else(|| unsafe { &*create_info });
            let ring = dispatch
                .as_ref()
                .and_then(|entry| entry.capture.as_ref())
                .and_then(|capture| {
                    match ExportImageRing::create_with_function(
                        capture.device.clone(),
                        capture.get_memory_win32_handle,
                        &capture.memory_properties,
                        ExportImageRingDesc {
                            extent: info.image_extent,
                            format: info.image_format,
                            slots: 3,
                        },
                    ) {
                        Ok(ring) => {
                            let handles = ring
                                .slots()
                                .iter()
                                .map(|slot| slot.raw_handle())
                                .collect::<Vec<_>>();
                            layer_log(format!(
                                "snowglass: attached {}-slot export ring {:?}",
                                ring.slots().len(),
                                handles
                            ));
                            Some(ring)
                        }
                        Err(error) => {
                            layer_log(format!("snowglass: export ring allocation failed: {error}"));
                            None
                        }
                    }
                });
            let timeline = ring.as_ref().and_then(|_| {
                let capture = dispatch.as_ref()?.capture.as_ref()?;
                match ExportTimelineSemaphore::create_with_function(
                    capture.device.clone(),
                    capture.get_semaphore_win32_handle,
                ) {
                    Ok(timeline) => Some(timeline),
                    Err(error) => {
                        layer_log(format!(
                            "snowglass: export timeline semaphore creation failed: {error}"
                        ));
                        None
                    }
                }
            });
            let ipc = ring
                .as_ref()
                .zip(timeline.as_ref())
                .and_then(|(ring, timeline)| {
                    connect_viewer(ring, timeline, info.image_extent, info.image_format)
                });
            unsafe {
                SWAPCHAIN.insert(
                    *swapchain,
                    SwapchainState {
                        device,
                        format: info.image_format,
                        extent: info.image_extent,
                        images: Vec::new(),
                        ring,
                        timeline,
                        copy_engine: None,
                        ipc,
                        frame_number: 0,
                    },
                );
            }
        }
        layer_log(format!(
            "snowglass: intercepted vkCreateSwapchainKHR (count={count})"
        ));
    }
    result
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDestroySwapchainKHR(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    unsafe { SWAPCHAIN.remove(&swapchain) };
    let dispatch = unsafe { dispatch_key(device) }
        .and_then(|key| unsafe { DEVICE.get(&key).map(|entry| entry.clone()) });
    if let Some(function) = dispatch.and_then(|entry| entry.destroy_swapchain) {
        unsafe { function(device, swapchain, allocator) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetSwapchainImagesKHR(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    image_count: *mut u32,
    images: *mut vk::Image,
) -> vk::Result {
    let dispatch = unsafe { dispatch_key(device) }
        .and_then(|key| unsafe { DEVICE.get(&key).map(|entry| entry.clone()) });
    let result = dispatch
        .and_then(|entry| entry.get_swapchain_images)
        .map_or(vk::Result::ERROR_EXTENSION_NOT_PRESENT, |function| unsafe {
            function(device, swapchain, image_count, images)
        });
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !image_count.is_null()
        && !images.is_null()
    {
        let captured =
            unsafe { std::slice::from_raw_parts(images, *image_count as usize).to_vec() };
        if let Some(mut state) = unsafe { SWAPCHAIN.get_mut(&swapchain) } {
            state.images = captured;
            layer_log(format!(
                "snowglass: associated {} swapchain images ({}x{}, format={})",
                state.images.len(),
                state.extent.width,
                state.extent.height,
                state.format.as_raw()
            ));
        }
    }
    result
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkQueuePresentKHR(
    queue: vk::Queue,
    present_info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    let queue_state = unsafe { QUEUE.get(&queue).map(|entry| entry.clone()) };
    let Some(present_function) = queue_state
        .as_ref()
        .and_then(|entry| entry.dispatch.queue_present)
    else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };

    let result = if let Some(queue_state) = queue_state {
        if !present_info.is_null()
            && unsafe { (*present_info).swapchain_count } == 1
            && !unsafe { (*present_info).p_swapchains }.is_null()
            && !unsafe { (*present_info).p_image_indices }.is_null()
        {
            let original = unsafe { &*present_info };
            let swapchain = unsafe { *original.p_swapchains };
            let image_index = unsafe { *original.p_image_indices };
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
            let copy_ready = unsafe { SWAPCHAIN.get_mut(&swapchain) }.and_then(|mut state| {
                if state.copy_engine.is_none() {
                    let ring = state.ring.take()?;
                    let timeline = state.timeline.take()?;
                    match SwapchainCopyEngine::create(
                        queue_state.dispatch.capture.as_ref()?.device.clone(),
                        ring,
                        timeline,
                        state.images.clone(),
                        state.extent,
                        queue_state.queue_family_index,
                        queue_state
                            .dispatch
                            .capture
                            .as_ref()
                            .and_then(|capture| capture.set_loader_data),
                    ) {
                        Ok(engine) => state.copy_engine = Some(engine),
                        Err(error) => {
                            layer_log(format!("snowglass: copy engine creation failed: {error}"));
                            return None;
                        }
                    }
                }
                match state
                    .copy_engine
                    .as_mut()?
                    .submit_copy(queue, image_index, waits)
                {
                    Ok(submission) => {
                        let count = COPIES_SUBMITTED.fetch_add(1, Ordering::Relaxed) + 1;
                        state.frame_number += 1;
                        let notification = Message::FrameReady(FrameReady {
                            stream_id: StreamId(swapchain.as_raw()),
                            slot: submission.slot,
                            _reserved: 0,
                            frame_number: state.frame_number,
                            sync_value: submission.sync_value,
                            timestamp_qpc: performance_counter().unwrap_or(0),
                        });
                        if state
                            .ipc
                            .as_mut()
                            .is_some_and(|socket| socket.send(notification).is_err())
                        {
                            layer_log("snowglass: viewer socket disconnected");
                            state.ipc = None;
                        }
                        if count == 1 || count % 120 == 0 {
                            layer_log(format!(
                                "snowglass: submitted swapchain copy (count={count})"
                            ));
                        }
                        Some(submission.present_ready)
                    }
                    Err(error) => {
                        layer_log(format!("snowglass: swapchain copy failed: {error}"));
                        None
                    }
                }
            });

            if let Some(copy_ready) = copy_ready {
                let copy_waits = [copy_ready];
                let mut modified = *original;
                modified.wait_semaphore_count = 1;
                modified.p_wait_semaphores = copy_waits.as_ptr();
                unsafe { present_function(queue, &modified) }
            } else {
                unsafe { present_function(queue, present_info) }
            }
        } else {
            unsafe { present_function(queue, present_info) }
        }
    } else {
        vk::Result::ERROR_DEVICE_LOST
    };
    let count = PRESENTS_SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 || count % 120 == 0 {
        layer_log(format!(
            "snowglass: intercepted vkQueuePresentKHR (count={count})"
        ));
    }
    result
}
