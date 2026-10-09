use super::*;
use polymer_presentation::producer::ProducerWindows;
mod capabilities;
mod profile;
use crate::layer::PhysicalDeviceState;
use parking_lot::Mutex;
use polymer_platform_windows::{NativeWindow, WindowLifetime};
use std::{
    ffi::CStr,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);
static ASSIGNMENTS: LazyLock<Result<Arc<AssignmentRegistry>, String>> = LazyLock::new(|| {
    AssignmentRegistry::new(polymer_hook::config::render_configuration().map_err(str::to_owned)?)
});

pub(crate) struct Surface {
    source: SourceIdentity,
    pub window: NativeWindow,
    lifetime: u64,
    registration: SourceRegistration,
    binding: Mutex<(Option<ProvisionalBinding>, Option<Arc<FixedSurfaceBinding>>)>,
}
pub(super) struct Profile {
    pub formats: Vec<vk::SurfaceFormatKHR>,
    pub usage: vk::ImageUsageFlags,
    pub max_extent: vk::Extent2D,
}
impl Surface {
    pub(crate) fn created(instance: vk::Instance, handle: vk::SurfaceKHR, window: isize) {
        if !enabled() {
            return;
        }
        let window = NativeWindow(window);
        let Ok(lifetime) = WindowLifetime::identify(window) else {
            return;
        };
        let Ok(registry) = ASSIGNMENTS.as_ref() else {
            return;
        };
        let Ok(serial) = NEXT_SOURCE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        }) else {
            return;
        };
        let source = SourceIdentity {
            device: instance.as_raw(),
            object: serial,
            lifetime: serial,
        };
        let candidate = Candidate {
            source,
            window: WindowIdentity {
                handle: window.0 as u64,
                lifetime,
            },
            title: polymer_platform_windows::window_title(window).unwrap_or_default(),
        };
        match registry.register(candidate) {
            Ok(registration) => {
                SURFACES.insert(
                    handle,
                    Arc::new(Self {
                        source,
                        window,
                        lifetime,
                        registration,
                        binding: Mutex::new((None, None)),
                    }),
                );
            }
            Err(error) => tracing::warn!(error, "virtual Vulkan surface not registered"),
        }
    }
    pub(crate) fn destroyed(handle: vk::SurfaceKHR) {
        if let Some((_, surface)) = SURFACES.remove(&handle) {
            surface.registration.release();
        }
    }
    pub(super) fn get(handle: vk::SurfaceKHR) -> Option<Arc<Self>> {
        SURFACES.get(&handle).map(|v| v.clone())
    }
    pub(super) fn binding(&self) -> Option<Arc<FixedSurfaceBinding>> {
        if WindowLifetime::identify(self.window).ok() != Some(self.lifetime) {
            return None;
        }
        if let Ok(registry) = ASSIGNMENTS.as_ref() {
            registry.observe([Candidate {
                source: self.source,
                window: WindowIdentity {
                    handle: self.window.0 as u64,
                    lifetime: self.lifetime,
                },
                title: polymer_platform_windows::window_title(self.window).unwrap_or_default(),
            }]);
        }
        let mut state = self.binding.lock();
        if let Some(binding) = &state.1 {
            return Some(binding.clone());
        }
        if state.0.is_none() {
            state.0 = self
                .registration
                .reserve(ProducerWindows::client_extent(self.window).ok()?);
        }
        state.0.as_ref().map(ProvisionalBinding::shared_binding)
    }
    pub(super) fn generation(
        &self,
        extent: polymer_presentation::Extent2D,
    ) -> Option<ProvisionalBinding> {
        let reservation = self.binding.lock().0.take();
        if let Some(reservation) = reservation {
            if reservation.shared_binding().extent() == extent {
                return Some(reservation);
            }
            drop(reservation);
        }
        self.registration.next_generation(extent)
    }
    pub(super) fn commit(&self, binding: Arc<FixedSurfaceBinding>) {
        self.binding.lock().1 = Some(binding);
    }
    pub(super) fn extent(&self) -> Option<polymer_presentation::Extent2D> {
        ProducerWindows::client_extent(self.window)
            .ok()
            .or_else(|| self.binding().map(|binding| binding.extent()))
    }
    pub(super) fn detached(
        &self,
        extent: polymer_presentation::Extent2D,
    ) -> Option<Arc<FixedSurfaceBinding>> {
        self.registration.detached(extent)
    }
    pub(super) unsafe fn proc<T: Copy>(physical: vk::PhysicalDevice, name: &CStr) -> Option<T> {
        let state = crate::layer::physical_device_state(physical)?;
        unsafe { crate::terminal::instance_proc(state.next_gipa, state.instance, name) }
    }
    pub(super) fn queue_properties(
        physical: vk::PhysicalDevice,
        state: &PhysicalDeviceState,
    ) -> Result<Vec<vk::QueueFamilyProperties>, vk::Result> {
        let get: vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties = unsafe {
            crate::terminal::instance_proc(
                state.next_gipa,
                state.instance,
                c"vkGetPhysicalDeviceQueueFamilyProperties",
            )
        }
        .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let mut count = 0;
        unsafe {
            get(physical, &mut count, std::ptr::null_mut());
        }
        let mut properties = vec![vk::QueueFamilyProperties::default(); count as usize];
        unsafe {
            get(physical, &mut count, properties.as_mut_ptr());
        }
        properties.truncate(count as usize);
        Ok(properties)
    }
}

pub(super) fn hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    crate::hook(match name {
        b"vkGetPhysicalDeviceSurfaceSupportKHR" => {
            vkGetPhysicalDeviceSurfaceSupportKHR as *const ()
        }
        b"vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => {
            vkGetPhysicalDeviceSurfaceCapabilitiesKHR as *const ()
        }
        b"vkGetPhysicalDeviceSurfaceCapabilities2KHR" => {
            vkGetPhysicalDeviceSurfaceCapabilities2KHR as *const ()
        }
        b"vkGetPhysicalDeviceSurfaceFormatsKHR" => {
            vkGetPhysicalDeviceSurfaceFormatsKHR as *const ()
        }
        b"vkGetPhysicalDeviceSurfaceFormats2KHR" => {
            vkGetPhysicalDeviceSurfaceFormats2KHR as *const ()
        }
        b"vkGetPhysicalDeviceSurfacePresentModesKHR" => {
            vkGetPhysicalDeviceSurfacePresentModesKHR as *const ()
        }
        _ => return None,
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetPhysicalDeviceSurfaceSupportKHR(
    physical: vk::PhysicalDevice,
    family: u32,
    surface: vk::SurfaceKHR,
    output: *mut vk::Bool32,
) -> vk::Result {
    if Surface::get(surface).is_some() {
        let supported = crate::layer::physical_device_state(physical)
            .and_then(|state| Surface::queue_properties(physical, &state).ok())
            .and_then(|p| p.get(family as usize).copied())
            .is_some_and(|p| p.queue_flags.contains(vk::QueueFlags::GRAPHICS));
        unsafe {
            *output = if supported && Surface::profile(physical).is_ok() {
                vk::TRUE
            } else {
                vk::FALSE
            };
        }
        return vk::Result::SUCCESS;
    }
    let Some(next): Option<vk::PFN_vkGetPhysicalDeviceSurfaceSupportKHR> =
        (unsafe { Surface::proc(physical, c"vkGetPhysicalDeviceSurfaceSupportKHR") })
    else {
        return vk::Result::ERROR_SURFACE_LOST_KHR;
    };
    unsafe { next(physical, family, surface, output) }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetPhysicalDeviceSurfaceCapabilitiesKHR(
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
    output: *mut vk::SurfaceCapabilitiesKHR,
) -> vk::Result {
    if let Some(surface) = Surface::get(surface) {
        return match surface
            .extent()
            .ok_or(vk::Result::ERROR_SURFACE_LOST_KHR)
            .and_then(|extent| surface.capabilities(physical, extent))
        {
            Ok(value) => {
                unsafe {
                    *output = value;
                }
                vk::Result::SUCCESS
            }
            Err(e) => e,
        };
    }
    let Some(next): Option<vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR> =
        (unsafe { Surface::proc(physical, c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR") })
    else {
        return vk::Result::ERROR_SURFACE_LOST_KHR;
    };
    unsafe { next(physical, surface, output) }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetPhysicalDeviceSurfaceCapabilities2KHR(
    physical: vk::PhysicalDevice,
    info: *const vk::PhysicalDeviceSurfaceInfo2KHR<'_>,
    output: *mut vk::SurfaceCapabilities2KHR<'_>,
) -> vk::Result {
    if Surface::get(unsafe { (*info).surface }).is_some() {
        return unsafe { capabilities::Capabilities::query(physical, &*info, &mut *output) };
    }
    let Some(next): Option<vk::PFN_vkGetPhysicalDeviceSurfaceCapabilities2KHR> =
        (unsafe { Surface::proc(physical, c"vkGetPhysicalDeviceSurfaceCapabilities2KHR") })
    else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    unsafe { next(physical, info, output) }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetPhysicalDeviceSurfaceFormatsKHR(
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
    count: *mut u32,
    output: *mut vk::SurfaceFormatKHR,
) -> vk::Result {
    if Surface::get(surface).is_some() {
        return match Surface::profile(physical) {
            Ok(profile) => unsafe { WsiResult::enumerate(&profile.formats, count, output) },
            Err(e) => e,
        };
    }
    let Some(next): Option<vk::PFN_vkGetPhysicalDeviceSurfaceFormatsKHR> =
        (unsafe { Surface::proc(physical, c"vkGetPhysicalDeviceSurfaceFormatsKHR") })
    else {
        return vk::Result::ERROR_SURFACE_LOST_KHR;
    };
    unsafe { next(physical, surface, count, output) }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetPhysicalDeviceSurfaceFormats2KHR(
    physical: vk::PhysicalDevice,
    info: *const vk::PhysicalDeviceSurfaceInfo2KHR<'_>,
    count: *mut u32,
    output: *mut vk::SurfaceFormat2KHR<'_>,
) -> vk::Result {
    if Surface::get(unsafe { (*info).surface }).is_some() {
        if unsafe { !(*info).p_next.is_null() } {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        }
        let profile = match Surface::profile(physical) {
            Ok(p) => p,
            Err(e) => return e,
        };
        if output.is_null() {
            unsafe {
                *count = profile.formats.len() as u32;
            }
            return vk::Result::SUCCESS;
        }
        let copied = unsafe { (*count as usize).min(profile.formats.len()) };
        for (index, format) in profile.formats.iter().take(copied).enumerate() {
            if unsafe { !(*output.add(index)).p_next.is_null() } {
                return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
            }
            unsafe {
                (*output.add(index)).surface_format = *format;
            }
        }
        unsafe {
            *count = copied as u32;
        }
        return if copied < profile.formats.len() {
            vk::Result::INCOMPLETE
        } else {
            vk::Result::SUCCESS
        };
    }
    let Some(next): Option<vk::PFN_vkGetPhysicalDeviceSurfaceFormats2KHR> =
        (unsafe { Surface::proc(physical, c"vkGetPhysicalDeviceSurfaceFormats2KHR") })
    else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    unsafe { next(physical, info, count, output) }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetPhysicalDeviceSurfacePresentModesKHR(
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
    count: *mut u32,
    output: *mut vk::PresentModeKHR,
) -> vk::Result {
    if Surface::get(surface).is_some() {
        return unsafe {
            WsiResult::enumerate(
                &[vk::PresentModeKHR::FIFO, vk::PresentModeKHR::IMMEDIATE],
                count,
                output,
            )
        };
    }
    let Some(next): Option<vk::PFN_vkGetPhysicalDeviceSurfacePresentModesKHR> =
        (unsafe { Surface::proc(physical, c"vkGetPhysicalDeviceSurfacePresentModesKHR") })
    else {
        return vk::Result::ERROR_SURFACE_LOST_KHR;
    };
    unsafe { next(physical, surface, count, output) }
}
