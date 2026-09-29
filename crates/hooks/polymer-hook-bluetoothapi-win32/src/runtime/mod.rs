use crate::{enumeration::SyntheticEnumeration, handles::SyntheticHandles};
use polymer_hook_input::HookInput;
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
            Properties::{
                DEVPKEY_Device_EnumeratorName, DEVPKEY_Device_InstanceId, DEVPROP_TYPE_STRING,
                DEVPROPTYPE,
            },
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
        polymer_hook::telemetry::init_file_tracing(polymer_hook::config::log_file(
            polymer_hook::config::BLUETOOTH_API_LOG_FILE,
        ));
        let session = polymer_hook::config::session().ok();
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
            "updated Polymer virtual Bluetooth HID topology"
        );
        self.topology_generation
            .store(topology.generation(), Ordering::Release);
    }

    fn is_bluetooth_interface(path: &[u16]) -> bool {
        use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
            CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent,
            CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_BUFFER_SMALL,
        };

        let path = path.iter().copied().chain(Some(0)).collect::<Vec<_>>();
        let mut property_type: DEVPROPTYPE = 0;
        let mut byte_len = 0u32;
        let result = unsafe {
            CM_Get_Device_Interface_PropertyW(
                path.as_ptr(),
                &DEVPKEY_Device_InstanceId,
                &mut property_type,
                std::ptr::null_mut(),
                &mut byte_len,
                0,
            )
        };
        if result != CR_BUFFER_SMALL || byte_len < 2 {
            return false;
        }
        let mut instance_id = vec![0u16; byte_len.div_ceil(2) as usize];
        if unsafe {
            CM_Get_Device_Interface_PropertyW(
                path.as_ptr(),
                &DEVPKEY_Device_InstanceId,
                &mut property_type,
                instance_id.as_mut_ptr().cast(),
                &mut byte_len,
                0,
            )
        } != CR_SUCCESS
            || property_type != DEVPROP_TYPE_STRING
        {
            return false;
        }

        let mut device = 0u32;
        if unsafe {
            CM_Locate_DevNodeW(
                &mut device,
                instance_id.as_mut_ptr(),
                CM_LOCATE_DEVNODE_NORMAL,
            )
        } != CR_SUCCESS
        {
            return false;
        }
        for _ in 0..16 {
            property_type = 0;
            byte_len = 0;
            if unsafe {
                CM_Get_DevNode_PropertyW(
                    device,
                    &DEVPKEY_Device_EnumeratorName,
                    &mut property_type,
                    std::ptr::null_mut(),
                    &mut byte_len,
                    0,
                )
            } == CR_BUFFER_SMALL
                && byte_len >= 2
            {
                let mut enumerator = vec![0u16; byte_len.div_ceil(2) as usize];
                if unsafe {
                    CM_Get_DevNode_PropertyW(
                        device,
                        &DEVPKEY_Device_EnumeratorName,
                        &mut property_type,
                        enumerator.as_mut_ptr().cast(),
                        &mut byte_len,
                        0,
                    )
                } == CR_SUCCESS
                    && property_type == DEVPROP_TYPE_STRING
                {
                    let name = String::from_utf16_lossy(&enumerator).to_ascii_uppercase();
                    if name.starts_with("BTH") || name.starts_with("BLUETOOTH") {
                        return true;
                    }
                }
            }
            let mut parent = 0u32;
            if unsafe { CM_Get_Parent(&mut parent, device, 0) } != CR_SUCCESS {
                break;
            }
            device = parent;
        }
        false
    }
}

pub(crate) unsafe fn bluetooth_find_first_device(
    search: *const BLUETOOTH_DEVICE_SEARCH_PARAMS,
    device: *mut BLUETOOTH_DEVICE_INFO,
    mut next: crate::hooks::BluetoothFindFirstDeviceContext<'_>,
) -> HBLUETOOTH_DEVICE_FIND {
    let state = next.state.clone();
    state.synchronize_topology(true);
    if state.session.is_none() {
        return polymer_hook::next!(next, search, device);
    }
    if !state
        .logged_bluetooth_suppression
        .swap(true, Ordering::AcqRel)
    {
        tracing::info!("isolated the emulator from native Windows Bluetooth discovery");
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
    let result = polymer_hook::next!(next, length, interface, device_id, flags);
    if result == CR_SUCCESS
        && device_id.is_null()
        && unsafe { state.enumeration.supports(interface) }
        && !length.is_null()
    {
        if !state.logged_enumeration.swap(true, Ordering::AcqRel) {
            tracing::info!("filtered native Bluetooth HID discovery behind Polymer devices");
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
    let result = polymer_hook::next!(next, interface, device_id, buffer, buffer_len, flags);
    if result == CR_SUCCESS
        && device_id.is_null()
        && unsafe { state.enumeration.supports(interface) }
    {
        return unsafe {
            state
                .enumeration
                .filter_and_append(buffer, buffer_len, |path| {
                    !Runtime::is_bluetooth_interface(path)
                })
        };
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
        return polymer_hook::next!(
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
        if !state.logged_attribute_probe.swap(true, Ordering::AcqRel) {
            tracing::info!(
                active = device.active,
                product_id = format_args!("0x{product_id:04x}"),
                "Dolphin opened a synthetic Wii Remote for HID qualification"
            );
        }
        unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }
    } else {
        // Qualification keeps all four stable paths in Dolphin's cache. An
        // inactive placeholder still refuses the real data handle until a
        // live topology update binds its named pipe.
        if !device.active {
            unsafe { SetLastError(ERROR_FILE_NOT_FOUND) };
            return INVALID_HANDLE_VALUE;
        }
        unsafe { WaitNamedPipeW(pipe.as_ptr(), 250) };
        let handle = polymer_hook::next!(
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
        return polymer_hook::next!(next, handle, attributes);
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
    polymer_hook::next!(next, handle)
}
