use crate::{enumeration::SyntheticEnumeration, handles::SyntheticHandles};
use snowglass_hook_input::HookInput;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Devices::{
            Bluetooth::{
                BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS, HBLUETOOTH_DEVICE_FIND,
            },
            DeviceAndDriverInstallation::{CONFIGRET, CR_SUCCESS},
            HumanInterfaceDevice::HIDD_ATTRIBUTES,
        },
        Foundation::{
            ERROR_FILE_NOT_FOUND, GetLastError, HANDLE, INVALID_HANDLE_VALUE, SetLastError,
        },
        Storage::FileSystem::{FILE_FLAG_OVERLAPPED, OPEN_EXISTING},
        System::{Pipes::WaitNamedPipeW, Threading::CreateEventW},
    },
    core::{BOOL, GUID, PCWSTR, PWSTR},
};

pub(crate) struct Runtime {
    pub(crate) enumeration: SyntheticEnumeration,
    pub(crate) handles: SyntheticHandles,
    input: Option<Arc<HookInput>>,
    session: Option<String>,
    topology_generation: AtomicU64,
    logged_enumeration: AtomicBool,
    logged_bluetooth_suppression: AtomicBool,
    logged_attribute_probe: AtomicBool,
    logged_attributes: AtomicBool,
    logged_open: AtomicBool,
}

impl Default for Runtime {
    fn default() -> Self {
        snowglass_hook::telemetry::init_file_tracing(
            snowglass_hook::config::bluetooth_api_log_file(),
        );
        let session = snowglass_hook::config::session().ok();
        let enumeration = SyntheticEnumeration::default();
        if let Some(session) = &session {
            enumeration.rebind(session, &[]);
        }
        Self {
            enumeration,
            handles: SyntheticHandles::default(),
            input: session.as_ref().map(|session| HookInput::start(session)),
            session,
            topology_generation: AtomicU64::new(u64::MAX),
            logged_enumeration: AtomicBool::new(false),
            logged_bluetooth_suppression: AtomicBool::new(false),
            logged_attribute_probe: AtomicBool::new(false),
            logged_attributes: AtomicBool::new(false),
            logged_open: AtomicBool::new(false),
        }
    }
}

impl Runtime {
    fn synchronize_topology(&self, wait_for_initial: bool) {
        let Some(input) = &self.input else {
            return;
        };
        if wait_for_initial {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !input.topology().received() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let topology = input.topology();
        if self.topology_generation.load(Ordering::Acquire) == topology.generation() {
            return;
        }
        if let Some(session) = &self.session {
            self.enumeration
                .rebind(session, &topology.document().raw_hid);
        }
        tracing::info!(
            generation = topology.generation(),
            bindings = topology.document().raw_hid.len(),
            raw_hid = ?topology.document().raw_hid,
            "updated synthetic Wii Remote topology"
        );
        self.topology_generation
            .store(topology.generation(), Ordering::Release);
    }
}

pub(crate) unsafe fn bluetooth_find_first_device(
    search: *const BLUETOOTH_DEVICE_SEARCH_PARAMS,
    device: *mut BLUETOOTH_DEVICE_INFO,
    mut next: crate::hooks::BluetoothFindFirstDeviceContext<'_>,
) -> HBLUETOOTH_DEVICE_FIND {
    let state = next.state.clone();
    state.synchronize_topology(true);
    if !state.enumeration.has_devices() {
        return snowglass_hook::next!(next, search, device);
    }
    if !state
        .logged_bluetooth_suppression
        .swap(true, Ordering::AcqRel)
    {
        tracing::info!("suppressed Dolphin's Windows Bluetooth discovery for raw HID forwarding");
    }
    unsafe { SetLastError(windows_sys::Win32::Foundation::ERROR_NO_MORE_ITEMS) };
    std::ptr::null_mut()
}

pub(crate) unsafe fn interface_list_size(
    length: *mut u32,
    interface: *const GUID,
    device_id: PCWSTR,
    flags: u32,
    mut next: crate::hooks::CmGetDeviceInterfaceListSizeWContext<'_>,
) -> CONFIGRET {
    let state = next.state.clone();
    state.synchronize_topology(
        device_id.is_null() && unsafe { state.enumeration.supports(interface) },
    );
    let result = snowglass_hook::next!(next, length, interface, device_id, flags);
    if result == CR_SUCCESS
        && device_id.is_null()
        && unsafe { state.enumeration.extends(interface) }
        && !length.is_null()
    {
        if !state.logged_enumeration.swap(true, Ordering::AcqRel) {
            tracing::info!("Dolphin enumerated the synthetic Wii Remote HID interfaces");
        }
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
    state.synchronize_topology(
        device_id.is_null() && unsafe { state.enumeration.supports(interface) },
    );
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
    state.synchronize_topology(false);
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
    // Placeholder paths keep enumeration identity deterministic, but an
    // unbound placeholder must never qualify as a Nintendo HID device. If it
    // does, Dolphin can spend the real-Wiimote slot probing a device that has
    // no pipe while the active port is left unopened.
    if !device.active {
        unsafe { SetLastError(ERROR_FILE_NOT_FOUND) };
        return INVALID_HANDLE_VALUE;
    }
    let pipe = device.pipe.clone();
    let product_id = device.product_id;
    let handle = if desired_access == 0 {
        if !state.logged_attribute_probe.swap(true, Ordering::AcqRel) {
            tracing::info!(
                active = device.active,
                product_id = format_args!("0x{product_id:04x}"),
                "Dolphin opened a synthetic Wii Remote for HID qualification"
            );
        }
        unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }
    } else {
        unsafe { WaitNamedPipeW(pipe.as_ptr(), 250) };
        let handle = snowglass_hook::next!(
            next,
            pipe.as_ptr(),
            desired_access,
            share_mode,
            security,
            OPEN_EXISTING,
            attributes | FILE_FLAG_OVERLAPPED,
            template
        );
        if !state.logged_open.swap(true, Ordering::AcqRel) {
            if handle == INVALID_HANDLE_VALUE || handle.is_null() {
                tracing::warn!(
                    error = unsafe { GetLastError() },
                    product_id = format_args!("0x{product_id:04x}"),
                    "Dolphin failed to open the active synthetic Wii Remote pipe"
                );
            } else {
                tracing::info!(
                    product_id = format_args!("0x{product_id:04x}"),
                    "Dolphin opened the active synthetic Wii Remote pipe"
                );
            }
        }
        handle
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
    if !next.state.logged_attributes.swap(true, Ordering::AcqRel) {
        tracing::info!(
            product_id = format_args!("0x{product_id:04x}"),
            "qualified a synthetic HID handle as a Nintendo Wii Remote"
        );
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
