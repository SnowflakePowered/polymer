use super::*;
mod render;
mod render_pass;
mod surface;
use crate::layer::{DeviceDispatchTable, ExternalDevice, PhysicalDeviceState};
use std::{
    cell::RefCell,
    sync::atomic::{AtomicUsize, Ordering},
};
use windows::{
    Win32::{Foundation::HWND, UI::WindowsAndMessaging::*},
    core::PCWSTR,
};

pub(super) struct Driver {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    pub native: ash::Device,
    pub owner: Arc<Device>,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    errors: Box<AtomicUsize>,
    windows: RefCell<Vec<(HWND, vk::SurfaceKHR)>>,
}
impl Driver {
    pub(super) fn new() -> Self {
        unsafe {
            let entry = ash::Entry::load().unwrap();
            let application = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
            let mut extensions = vec![
                ash::khr::surface::NAME.as_ptr(),
                ash::khr::win32_surface::NAME.as_ptr(),
                ash::khr::get_surface_capabilities2::NAME.as_ptr(),
                ash::ext::surface_maintenance1::NAME.as_ptr(),
            ];
            let validation = entry
                .enumerate_instance_layer_properties()
                .unwrap()
                .iter()
                .any(|layer| {
                    std::ffi::CStr::from_ptr(layer.layer_name.as_ptr())
                        == c"VK_LAYER_KHRONOS_validation"
                });
            let layers = if validation {
                vec![c"VK_LAYER_KHRONOS_validation".as_ptr()]
            } else {
                Vec::new()
            };
            if validation {
                extensions.push(ash::ext::debug_utils::NAME.as_ptr());
            }
            let errors = Box::new(AtomicUsize::new(0));
            let instance = entry
                .create_instance(
                    &vk::InstanceCreateInfo::default()
                        .application_info(&application)
                        .enabled_extension_names(&extensions)
                        .enabled_layer_names(&layers),
                    None,
                )
                .unwrap();
            let debug = validation.then(|| {
                let loader = ash::ext::debug_utils::Instance::new(&entry, &instance);
                let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                    .message_severity(
                        vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                            | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                    )
                    .message_type(
                        vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                            | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                            | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                    )
                    .pfn_user_callback(Some(Self::validation))
                    .user_data((&*errors as *const AtomicUsize).cast_mut().cast());
                let messenger = loader.create_debug_utils_messenger(&info, None).unwrap();
                (loader, messenger)
            });
            eprintln!("Vulkan validation enabled: {validation}");
            let physical = instance.enumerate_physical_devices().unwrap()[0];
            let families = instance.get_physical_device_queue_family_properties(physical);
            let single = std::env::var("POLYMER_VULKAN_WSI_TEST_PROFILE").unwrap() == "exclusive";
            let graphics = families
                .iter()
                .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .unwrap();
            let priority = [1.0, 1.0];
            let queues = families
                .iter()
                .enumerate()
                .filter(|(index, p)| {
                    (!single || *index == graphics)
                        && p.queue_flags.intersects(
                            vk::QueueFlags::GRAPHICS
                                | vk::QueueFlags::COMPUTE
                                | vk::QueueFlags::TRANSFER,
                        )
                })
                .map(|(i, p)| {
                    vk::DeviceQueueCreateInfo::default()
                        .queue_family_index(i as u32)
                        .queue_priorities(&priority[..p.queue_count.min(2) as usize])
                })
                .collect::<Vec<_>>();
            let extensions = [
                ash::khr::swapchain::NAME,
                ash::khr::external_memory_win32::NAME,
                ash::khr::external_semaphore_win32::NAME,
                ash::khr::timeline_semaphore::NAME,
                ash::khr::synchronization2::NAME,
                ash::ext::queue_family_foreign::NAME,
                ash::ext::swapchain_maintenance1::NAME,
            ]
            .map(|name| name.as_ptr());
            let state = PhysicalDeviceState {
                instance: instance.handle(),
                next_gipa: entry.static_fn().get_instance_proc_addr,
            };
            let mut info = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&extensions);
            // Public maintenance requests are intentionally outside the virtual
            // profile; this extension is enabled internally for native bridges.
            let requested_extensions = extensions[..6].to_vec();
            let mut version12 = vk::PhysicalDeviceVulkan12Features::default()
                .timeline_semaphore(true)
                .imageless_framebuffer(true);
            let mut version13 =
                vk::PhysicalDeviceVulkan13Features::default().synchronization2(true);
            let mut imageless = vk::PhysicalDeviceImagelessFramebufferFeatures::default()
                .imageless_framebuffer(true);
            let mut requested = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&requested_extensions);
            if single {
                requested = requested
                    .push_next(&mut version12)
                    .push_next(&mut version13);
            }
            if !single {
                requested = requested.push_next(&mut imageless);
            }
            info.p_next = requested.p_next;
            let mut features =
                features::Features::new(physical, Some(&state), &requested, true).unwrap();
            features.attach(&mut info);
            let native = instance.create_device(physical, &info, None).unwrap();
            let memory = ash::khr::external_memory_win32::Device::new(&instance, &native);
            let semaphore = ash::khr::external_semaphore_win32::Device::new(&instance, &native);
            let mut id = vk::PhysicalDeviceIDProperties::default();
            instance.get_physical_device_properties2(
                physical,
                &mut vk::PhysicalDeviceProperties2::default().push_next(&mut id),
            );
            assert_eq!(id.device_luid_valid, vk::TRUE);
            let external = Arc::new(ExternalDevice {
                device: native.clone(),
                memory_properties: instance.get_physical_device_memory_properties(physical),
                get_memory_win32_handle_properties: memory
                    .fp()
                    .get_memory_win32_handle_properties_khr,
                import_semaphore_win32_handle: semaphore.fp().import_semaphore_win32_handle_khr,
                set_loader_data: None,
                supports_foreign_queue: true,
                adapter_luid: polymer_protocol::AdapterLuid {
                    low_part: u32::from_le_bytes(id.device_luid[..4].try_into().unwrap()),
                    high_part: i32::from_le_bytes(id.device_luid[4..].try_into().unwrap()),
                },
            });
            crate::layer::install_hook_state(Arc::default());
            crate::layer::hook_state()
                .physical_device
                .insert(physical, state.clone());
            crate::layer::register_device_dispatch(
                native.handle(),
                DeviceDispatchTable {
                    handle: native.handle(),
                    next_gdpa: instance.fp_v1_0().get_device_proc_addr,
                    physical_device: physical,
                    physical_state: Some(state.clone()),
                    external: Some(external),
                },
            );
            Device::install(native.handle(), &info).unwrap();
            let owner = Device::get(native.handle()).unwrap();
            eprintln!(
                "Vulkan WSI test uses {} queue families",
                owner.families.len()
            );
            Self {
                _entry: entry,
                instance,
                physical,
                native,
                owner,
                debug,
                errors,
                windows: RefCell::default(),
            }
        }
    }
    unsafe extern "system" fn validation(
        severity: vk::DebugUtilsMessageSeverityFlagsEXT,
        _kind: vk::DebugUtilsMessageTypeFlagsEXT,
        data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
        user: *mut std::ffi::c_void,
    ) -> vk::Bool32 {
        unsafe {
            if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
                (*(user.cast::<AtomicUsize>())).fetch_add(1, Ordering::Relaxed);
            }
            eprintln!(
                "Vulkan validation: {}",
                std::ffi::CStr::from_ptr((*data).p_message).to_string_lossy()
            );
        }
        vk::FALSE
    }
    pub(super) fn device(&self) -> vk::Device {
        self.native.handle()
    }
    unsafe fn hook<T: Copy>(&self, name: &[u8]) -> T {
        let function = device_hook(name).unwrap();
        unsafe { std::mem::transmute_copy(&function) }
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        assert!(
            Device::destroy(self.device()),
            "virtual device retirement failed"
        );
        unsafe {
            for (window, surface) in self.windows.borrow_mut().drain(..) {
                Surface::destroyed(surface);
                ash::khr::surface::Instance::new(&self._entry, &self.instance)
                    .destroy_surface(surface, None);
                let _ = DestroyWindow(window);
            }
            crate::layer::hook_state()
                .device
                .remove(&crate::layer::dispatch_key(self.device()).unwrap());
            crate::layer::hook_state()
                .queue
                .retain(|_, queue| queue.device != self.device());
            self.native.destroy_device(None);
            if let Some((loader, messenger)) = self.debug.take() {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
            self.instance.destroy_instance(None);
            assert_eq!(
                self.errors.load(Ordering::Relaxed),
                0,
                "Vulkan validation reported errors"
            );
        }
    }
}
