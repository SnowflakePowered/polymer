use ash::vk;
use std::{
    ffi::{CStr, c_char},
    ptr,
};

/// Borrows application extension names for the downstream create call and adds
/// only a complete, supported dependency set for presentation fences.
pub(crate) struct InstanceExtensions {
    pub names: Vec<*const c_char>,
    pub surface_maintenance: bool,
}

impl InstanceExtensions {
    pub(crate) unsafe fn negotiate(
        info: &vk::InstanceCreateInfo<'_>,
        enumerate: Option<vk::PFN_vkEnumerateInstanceExtensionProperties>,
    ) -> Self {
        let mut available = Vec::new();
        if let Some(enumerate) = enumerate {
            let mut count = 0;
            if unsafe { enumerate(ptr::null(), &mut count, ptr::null_mut()) } == vk::Result::SUCCESS
            {
                available.resize(count as usize, vk::ExtensionProperties::default());
                let result = unsafe { enumerate(ptr::null(), &mut count, available.as_mut_ptr()) };
                if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE) {
                    available.truncate(count as usize);
                } else {
                    available.clear();
                }
            }
        }
        let names = available
            .iter()
            .map(|property| unsafe { CStr::from_ptr(property.extension_name.as_ptr()) })
            .collect::<Vec<_>>();
        unsafe { Self::from_available(info, &names) }
    }

    unsafe fn from_available(info: &vk::InstanceCreateInfo<'_>, available: &[&CStr]) -> Self {
        let mut names = if info.enabled_extension_count == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    info.pp_enabled_extension_names,
                    info.enabled_extension_count as usize,
                )
            }
            .to_vec()
        };
        let enabled = |required: &CStr| {
            names
                .iter()
                .any(|name| !name.is_null() && unsafe { CStr::from_ptr(*name) } == required)
        };
        let mut dependencies = vec![
            ash::khr::get_surface_capabilities2::NAME,
            ash::ext::surface_maintenance1::NAME,
        ];
        let version = unsafe { info.p_application_info.as_ref() }
            .map_or(vk::API_VERSION_1_0, |app| app.api_version);
        if version < vk::API_VERSION_1_1 {
            dependencies.push(ash::khr::get_physical_device_properties2::NAME);
        }
        let supported = enabled(ash::khr::surface::NAME)
            && dependencies
                .iter()
                .all(|required| enabled(required) || available.contains(required));
        if supported {
            for required in dependencies {
                if !names
                    .iter()
                    .any(|name| !name.is_null() && unsafe { CStr::from_ptr(*name) } == required)
                {
                    names.push(required.as_ptr());
                }
            }
        }
        let surface_maintenance = names.iter().any(|name| {
            !name.is_null()
                && unsafe { CStr::from_ptr(*name) } == ash::ext::surface_maintenance1::NAME
        });
        Self {
            names,
            surface_maintenance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_dependencies_are_atomic_and_preserve_application_extensions() {
        unsafe {
            let requested = [
                ash::khr::surface::NAME.as_ptr(),
                ash::khr::win32_surface::NAME.as_ptr(),
            ];
            let info = vk::InstanceCreateInfo::default().enabled_extension_names(&requested);
            let mut available = vec![
                ash::khr::get_surface_capabilities2::NAME,
                ash::ext::surface_maintenance1::NAME,
            ];
            let unsupported = InstanceExtensions::from_available(&info, &available);
            assert_eq!(unsupported.names, requested);
            assert!(!unsupported.surface_maintenance);
            available.push(ash::khr::get_physical_device_properties2::NAME);
            let supported = InstanceExtensions::from_available(&info, &available);
            assert!(supported.surface_maintenance);
            assert_eq!(&supported.names[..2], &requested);
            assert_eq!(supported.names.len(), 5);
            let existing =
                vk::InstanceCreateInfo::default().enabled_extension_names(&supported.names);
            assert_eq!(
                InstanceExtensions::from_available(&existing, &available).names,
                supported.names
            );
            let headless =
                InstanceExtensions::from_available(&vk::InstanceCreateInfo::default(), &available);
            assert!(headless.names.is_empty());
            let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
            let modern = info.application_info(&app);
            assert_eq!(
                InstanceExtensions::from_available(&modern, &available[..2])
                    .names
                    .len(),
                4
            );
        }
    }
}
