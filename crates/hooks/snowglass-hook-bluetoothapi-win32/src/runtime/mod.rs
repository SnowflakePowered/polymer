use crate::{enumeration::SyntheticEnumeration, handles::SyntheticHandles};
use snowglass_hook_input::HookInput;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use windows_sys::{
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{CONFIGRET, CR_SUCCESS},
            HumanInterfaceDevice::HIDD_ATTRIBUTES,
        },
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{FILE_FLAG_OVERLAPPED, OPEN_EXISTING},
        System::{Pipes::WaitNamedPipeW, Threading::CreateEventW},
    },
    core::{BOOL, GUID, PCWSTR, PWSTR},
};

pub(crate) struct Runtime {
    pub(crate) enumeration: SyntheticEnumeration,
    pub(crate) handles: SyntheticHandles,
    input: Option<Arc<HookInput>>,
    revision: AtomicU32,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            enumeration: SyntheticEnumeration::default(),
            handles: SyntheticHandles::default(),
            input: snowglass_hook::config::session()
                .ok()
                .map(|session| HookInput::start(&session)),
            revision: AtomicU32::new(u32::MAX),
        }
    }
}

impl Runtime {
    fn synchronize_topology(&self) {
        let Some(input) = &self.input else {
            return;
        };
        let topology = input.topology();
        if self.revision.load(Ordering::Acquire) == topology.revision() {
            return;
        }
        self.enumeration.rebind(&topology.document().raw_hid);
        self.revision.store(topology.revision(), Ordering::Release);
    }
}

pub(crate) unsafe fn interface_list_size(
    length: *mut u32,
    interface: *const GUID,
    device_id: PCWSTR,
    flags: u32,
    mut next: crate::hooks::CmGetDeviceInterfaceListSizeWContext<'_>,
) -> CONFIGRET {
    let state = next.state.clone();
    state.synchronize_topology();
    let result = snowglass_hook::next!(next, length, interface, device_id, flags);
    if result == CR_SUCCESS
        && device_id.is_null()
        && unsafe { state.enumeration.extends(interface) }
        && !length.is_null()
    {
        unsafe { *length = (*length).saturating_add(state.enumeration.additional_len()) };
    }
    result
}

pub(crate) unsafe fn interface_list(
    interface: *const GUID,
    device_id: PCWSTR,
    buffer: PWSTR,
    buffer_len: u32,
    flags: u32,
    mut next: crate::hooks::CmGetDeviceInterfaceListWContext<'_>,
) -> CONFIGRET {
    let state = next.state.clone();
    state.synchronize_topology();
    let result = snowglass_hook::next!(next, interface, device_id, buffer, buffer_len, flags);
    if result == CR_SUCCESS
        && device_id.is_null()
        && unsafe { state.enumeration.extends(interface) }
    {
        return unsafe { state.enumeration.append(buffer, buffer_len) };
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn create_file(
    path: PCWSTR,
    desired_access: u32,
    share_mode: u32,
    security: *const windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    creation: u32,
    attributes: u32,
    template: HANDLE,
    mut next: crate::hooks::CreateFileWContext<'_>,
) -> HANDLE {
    let state = next.state.clone();
    state.synchronize_topology();
    let Some(device) = (unsafe { state.enumeration.find_path(path) }) else {
        return snowglass_hook::next!(
            next,
            path,
            desired_access,
            share_mode,
            security,
            creation,
            attributes,
            template
        );
    };
    let pipe = device.pipe.clone();
    let product_id = device.product_id;
    let handle = if desired_access == 0 {
        unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }
    } else {
        unsafe { WaitNamedPipeW(pipe.as_ptr(), 250) };
        snowglass_hook::next!(
            next,
            pipe.as_ptr(),
            desired_access,
            share_mode,
            security,
            OPEN_EXISTING,
            attributes | FILE_FLAG_OVERLAPPED,
            template
        )
    };
    if handle != INVALID_HANDLE_VALUE && !handle.is_null() {
        state.handles.insert(handle, product_id);
    }
    handle
}

pub(crate) unsafe fn hid_attributes(
    handle: HANDLE,
    attributes: *mut HIDD_ATTRIBUTES,
    mut next: crate::hooks::HidDGetAttributesContext<'_>,
) -> bool {
    let Some(product_id) = next.state.handles.product_id(handle) else {
        return snowglass_hook::next!(next, handle, attributes);
    };
    if attributes.is_null() {
        return false;
    }
    unsafe {
        (*attributes).Size = size_of::<HIDD_ATTRIBUTES>() as u32;
        (*attributes).VendorID = 0x057e;
        (*attributes).ProductID = product_id;
        (*attributes).VersionNumber = 1;
    }
    true
}

pub(crate) unsafe fn close_handle(
    handle: HANDLE,
    mut next: crate::hooks::CloseHandleContext<'_>,
) -> BOOL {
    next.state.handles.remove(handle);
    snowglass_hook::next!(next, handle)
}
