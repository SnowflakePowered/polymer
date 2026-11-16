#![allow(clippy::too_many_arguments)]
use windows_sys::{
    Win32::{
        Devices::{
            Bluetooth::{
                BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS, BluetoothFindFirstDevice,
                HBLUETOOTH_DEVICE_FIND,
            },
            DeviceAndDriverInstallation::{
                CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CONFIGRET,
            },
            HumanInterfaceDevice::{HIDD_ATTRIBUTES, HidD_GetAttributes},
        },
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::CreateFileW,
    },
    core::{BOOL, GUID, PCWSTR, PWSTR},
};

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer(state = crate::runtime::Runtime)]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(target = BluetoothFindFirstDevice), fallback = std::ptr::null_mut())]
    #[infix(crate::runtime::bluetooth_find_first_device)]
    pub unsafe extern "system" fn bluetooth_find_first_device(
        search: *const BLUETOOTH_DEVICE_SEARCH_PARAMS,
        device: *mut BLUETOOTH_DEVICE_INFO,
    ) -> HBLUETOOTH_DEVICE_FIND;

    #[trampoline]
    #[terminal(installed(target = CM_Get_Device_Interface_List_SizeW), fallback = 1)]
    #[infix(crate::runtime::interface_list_size)]
    pub unsafe extern "system" fn cm_get_device_interface_list_size_w(
        length: *mut u32,
        interface: *const GUID,
        device_id: PCWSTR,
        flags: u32,
    ) -> CONFIGRET;

    #[trampoline]
    #[terminal(installed(target = CM_Get_Device_Interface_ListW), fallback = 1)]
    #[infix(crate::runtime::interface_list)]
    pub unsafe extern "system" fn cm_get_device_interface_list_w(
        interface: *const GUID,
        device_id: PCWSTR,
        buffer: PWSTR,
        buffer_len: u32,
        flags: u32,
    ) -> CONFIGRET;

    #[trampoline]
    #[terminal(installed(target = CreateFileW), fallback = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE)]
    #[infix(crate::runtime::create_file)]
    pub unsafe extern "system" fn create_file_w(
        path: PCWSTR,
        desired_access: u32,
        share_mode: u32,
        security: *const windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
        creation: u32,
        attributes: u32,
        template: HANDLE,
    ) -> HANDLE;

    #[trampoline]
    #[terminal(installed(target = HidD_GetAttributes), fallback = false)]
    #[infix(crate::runtime::hid_attributes)]
    pub unsafe extern "system" fn hid_d_get_attributes(
        handle: HANDLE,
        attributes: *mut HIDD_ATTRIBUTES,
    ) -> bool;

    #[trampoline]
    #[terminal(installed(target = CloseHandle), fallback = 0)]
    #[infix(crate::runtime::close_handle)]
    pub unsafe extern "system" fn close_handle(handle: HANDLE) -> BOOL;
}
