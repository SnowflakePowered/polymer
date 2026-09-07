//! Downstream Vulkan continuations owned by the hook framework.

use ash::vk;
use std::{
    ffi::{CStr, c_char, c_void},
    mem, ptr,
    sync::Arc,
};

const LOADER_LAYER_INTERFACE_VERSION: u32 = 2;

pub(crate) unsafe fn negotiate(
    interface: *mut crate::layer::VkNegotiateLayerInterface,
) -> vk::Result {
    polymer_hook::telemetry::init_file_tracing(polymer_hook::config::layer_log_file());
    let _ = crate::layer::install_hook_state(Arc::new(crate::layer::VulkanHookState::default()));
    if interface.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let requested = unsafe { (*interface).loader_layer_interface_version };
    if requested == 0 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    unsafe {
        (*interface).loader_layer_interface_version = requested.min(LOADER_LAYER_INTERFACE_VERSION);
        (*interface).pfn_get_instance_proc_addr = Some(crate::hooks::vkGetInstanceProcAddr);
        (*interface).pfn_get_device_proc_addr = Some(crate::hooks::vkGetDeviceProcAddr);
        (*interface).pfn_get_physical_device_proc_addr =
            Some(crate::hooks::vk_layerGetPhysicalDeviceProcAddr);
    }
    vk::Result::SUCCESS
}

pub(crate) unsafe fn get_instance_proc_addr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) };
    if let Some(hook) = crate::registered_instance_hook(name.to_bytes()) {
        return Some(hook);
    }
    crate::layer::instance_gipa(instance).and_then(|next| unsafe { next(instance, name.as_ptr()) })
}

pub(crate) unsafe fn get_physical_device_proc_addr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    if let Some(hook) =
        crate::swapchain::instance_hook(unsafe { CStr::from_ptr(name) }.to_bytes())
    {
        return Some(hook);
    }
    if let Some(next) = crate::layer::instance_physical_gpa(instance) {
        unsafe { next(instance, name) }
    } else {
        crate::layer::instance_gipa(instance).and_then(|next| unsafe { next(instance, name) })
    }
}

pub(crate) unsafe fn get_device_proc_addr(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) };
    if let Some(hook) = crate::registered_device_hook(name.to_bytes()) {
        return Some(hook);
    }
    crate::layer::device_gdpa(device).and_then(|next| unsafe { next(device, name.as_ptr()) })
}

pub(crate) unsafe fn instance_proc<T: Copy>(
    gipa: vk::PFN_vkGetInstanceProcAddr,
    instance: vk::Instance,
    name: &CStr,
) -> Option<T> {
    let function = unsafe { gipa(instance, name.as_ptr()) }?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&function));
    let pointer = &function as *const unsafe extern "system" fn();
    Some(unsafe { ptr::read(pointer.cast::<T>()) })
}

pub(crate) unsafe fn device_proc<T: Copy>(
    gdpa: vk::PFN_vkGetDeviceProcAddr,
    device: vk::Device,
    name: &CStr,
) -> Option<T> {
    let function = unsafe { gdpa(device, name.as_ptr()) }?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&function));
    let pointer = &function as *const unsafe extern "system" fn();
    Some(unsafe { ptr::read(pointer.cast::<T>()) })
}

unsafe fn device_supports_extension(
    state: &crate::layer::PhysicalDeviceState,
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
        && properties[..count as usize]
            .iter()
            .any(|property| unsafe { CStr::from_ptr(property.extension_name.as_ptr()) } == required)
}

pub(crate) unsafe fn create_instance(
    create_info: *const vk::InstanceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    instance: *mut vk::Instance,
) -> vk::Result {
    if let Err(error) = polymer_hook::config::require_swapchain() {
        tracing::error!(%error, "Vulkan presentation configuration rejected");
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let Some(bootstrap) = (unsafe { crate::layer::take_instance_bootstrap(create_info) }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next) = (unsafe {
        instance_proc::<vk::PFN_vkCreateInstance>(
            bootstrap.next_gipa,
            vk::Instance::null(),
            c"vkCreateInstance",
        )
    }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let mut modified = unsafe { *create_info };
    let mut extensions = if modified.enabled_extension_count == 0 {
        Vec::new()
    } else {
        unsafe {
            std::slice::from_raw_parts(
                modified.pp_enabled_extension_names,
                modified.enabled_extension_count as usize,
            )
        }
        .to_vec()
    };
    if crate::swapchain::enabled() {
        for required in [
            ash::khr::surface::NAME,
            ash::khr::get_surface_capabilities2::NAME,
            ash::ext::surface_maintenance1::NAME,
            ash::khr::get_physical_device_properties2::NAME,
        ] {
            if !extensions
                .iter()
                .any(|&name| unsafe { CStr::from_ptr(name) } == required)
            {
                extensions.push(required.as_ptr());
            }
        }
        modified.enabled_extension_count = extensions.len() as u32;
        modified.pp_enabled_extension_names = extensions.as_ptr();
    }
    let result = unsafe { next(&modified, allocator, instance) };
    if result == vk::Result::SUCCESS && !instance.is_null() {
        let created = unsafe { *instance };
        crate::layer::register_instance_dispatch(created, bootstrap);
    }
    result
}

pub(crate) unsafe fn create_device(
    physical_device: vk::PhysicalDevice,
    create_info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    let Some(bootstrap) = (unsafe { crate::layer::take_device_bootstrap(create_info) }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next_create) = (unsafe {
        instance_proc::<vk::PFN_vkCreateDevice>(
            bootstrap.next_gipa,
            vk::Instance::null(),
            c"vkCreateDevice",
        )
    }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let physical_state = crate::layer::physical_device_state(physical_device);
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
    let external_extensions = [
        ash::khr::external_memory_win32::NAME,
        ash::khr::external_semaphore_win32::NAME,
        ash::khr::timeline_semaphore::NAME,
    ];
    let foreign_queue_extension = ash::ext::queue_family_foreign::NAME;
    let mut injected_extensions = Vec::new();
    for required in external_extensions {
        let enabled = extension_names
            .iter()
            .any(|name| !name.is_null() && unsafe { CStr::from_ptr(*name) } == required);
        if !enabled
            && physical_state.as_ref().is_some_and(|state| unsafe {
                device_supports_extension(state, physical_device, required)
            })
        {
            extension_names.push(required.as_ptr());
            injected_extensions.push(required);
        }
    }
    let foreign_queue_enabled = extension_names
        .iter()
        .any(|name| !name.is_null() && unsafe { CStr::from_ptr(*name) } == foreign_queue_extension)
        || (physical_state.as_ref().is_some_and(|state| unsafe {
            device_supports_extension(state, physical_device, foreign_queue_extension)
        }) && {
            extension_names.push(foreign_queue_extension.as_ptr());
            injected_extensions.push(foreign_queue_extension);
            true
        });
    let external_supported = external_extensions.iter().all(|required| {
        extension_names
            .iter()
            .any(|name| !name.is_null() && unsafe { CStr::from_ptr(*name) } == *required)
    });
    if create_info.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    if unsafe { crate::swapchain::features::Features::virtual_device(&*create_info) } {
        for required in [
            ash::khr::synchronization2::NAME,
            ash::ext::swapchain_maintenance1::NAME,
        ] {
            if !extension_names
                .iter()
                .any(|name| unsafe { CStr::from_ptr(*name) } == required)
            {
                if !physical_state.as_ref().is_some_and(|state| unsafe {
                    device_supports_extension(state, physical_device, required)
                }) {
                    return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
                }
                extension_names.push(required.as_ptr());
                injected_extensions.push(required);
            }
        }
    }
    let mut features = match unsafe {
        crate::swapchain::features::Features::new(
            physical_device,
            physical_state.as_ref(),
            create_info,
            external_supported,
        )
    } {
        Ok(features) => features,
        Err(error) => return error,
    };
    let external_supported = features.supported;
    let mut modified = Some(unsafe { *create_info });
    if let Some(info) = modified.as_mut() {
        info.enabled_extension_count = extension_names.len() as u32;
        info.pp_enabled_extension_names = extension_names.as_ptr();
        features.attach(info);
    }
    let result = unsafe {
        next_create(
            physical_device,
            modified.as_ref().map_or(create_info, |info| info),
            allocator,
            device,
        )
    };
    if result != vk::Result::SUCCESS || device.is_null() {
        return result;
    }
    let created = unsafe { *device };
    let external = physical_state.as_ref().and_then(|state| {
        if !external_supported {
            return None;
        }
        let get_memory_win32_handle_properties = unsafe {
            device_proc(
                bootstrap.next_gdpa,
                created,
                c"vkGetMemoryWin32HandlePropertiesKHR",
            )
        }?;
        let import_semaphore_win32_handle = unsafe {
            device_proc(
                bootstrap.next_gdpa,
                created,
                c"vkImportSemaphoreWin32HandleKHR",
            )
        }?;
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
                    (bootstrap.next_gdpa)(created, name.as_ptr()).map_or(ptr::null(), |function| {
                        function as *const () as *const c_void
                    })
                },
                created,
            )
        };
        let adapter_luid = unsafe {
            instance_proc::<vk::PFN_vkGetPhysicalDeviceProperties2>(
                state.next_gipa,
                state.instance,
                c"vkGetPhysicalDeviceProperties2",
            )
        }
        .and_then(|get_properties2| {
            let mut id = vk::PhysicalDeviceIDProperties::default();
            let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
            unsafe { get_properties2(physical_device, &mut properties) };
            (id.device_luid_valid == vk::TRUE).then(|| {
                let bytes = id.device_luid;
                polymer_protocol::AdapterLuid {
                    low_part: u32::from_le_bytes(bytes[0..4].try_into().expect("LUID size")),
                    high_part: i32::from_le_bytes(bytes[4..8].try_into().expect("LUID size")),
                }
            })
        })
        .unwrap_or(polymer_protocol::AdapterLuid {
            low_part: 0,
            high_part: 0,
        });
        Some(Arc::new(crate::layer::ExternalDevice {
            device: ash_device,
            memory_properties,
            get_memory_win32_handle_properties,
            import_semaphore_win32_handle,
            set_loader_data: bootstrap.set_loader_data,
            supports_foreign_queue: foreign_queue_enabled,
            adapter_luid,
        }))
    });
    crate::layer::observe_create_device(injected_extensions.len());
    crate::layer::register_device_dispatch(
        created,
        crate::layer::DeviceDispatchTable {
            next_gdpa: bootstrap.next_gdpa,
            physical_device,
            physical_state,
            external,
            handle: created,
        },
    );
    if let Err(error) = unsafe { crate::swapchain::Device::install(created, &*create_info) }
    {
        let dispatch_key = unsafe { crate::layer::dispatch_key(created) };
        if let Some(destroy) = unsafe {
            device_proc::<vk::PFN_vkDestroyDevice>(bootstrap.next_gdpa, created, c"vkDestroyDevice")
        } {
            unsafe {
                destroy(created, allocator);
            }
        }
        if let Some(key) = dispatch_key {
            crate::layer::hook_state().device.remove(&key);
        }
        crate::layer::hook_state()
            .queue
            .retain(|_, dispatch| dispatch.device != created);
        unsafe {
            *device = vk::Device::null();
        }
        return error;
    }
    result
}
