use std::mem::size_of;

use polymer_bluetooth::{Error, Result};
use windows::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS},
        System::{Registry::*, SystemInformation::GetWindowsDirectoryW},
    },
    core::{BOOL, PCWSTR, w},
};

/// Owns a device information set and its driver lists for one switch attempt.
pub(super) struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: this handle is uniquely owned and was created by SetupAPI.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

impl DeviceSet {
    pub fn switch(path: &str) -> Result<String> {
        // SAFETY: no optional pointers or borrowed Windows handles are supplied.
        let set = Self(
            unsafe { SetupDiCreateDeviceInfoList(None, None) }
                .map_err(|error| Self::error("create device information set", error))?,
        );
        let instance = set.usb_parent(path)?;
        let wide = instance.encode_utf16().chain([0]).collect::<Vec<_>>();
        let mut device = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: the string is terminated and device is a correctly sized output.
        unsafe { SetupDiOpenDeviceInfoW(set.0, PCWSTR(wide.as_ptr()), None, 0, Some(&mut device)) }
            .map_err(|error| Self::error("open selected DS3 USB instance", error))?;
        let original_class = device.ClassGuid;
        set.set_class(
            &mut device,
            windows::core::GUID::from_u128(0x88bae032_5a81_49f0_bc3d_a4ff138216d6),
        )?;
        let installation = set.install(&mut device);
        let reboot = match installation {
            Ok(reboot) => reboot,
            Err(error) => {
                return match set.set_class(&mut device, original_class) {
                    Ok(()) => Err(error),
                    Err(restore) => Err(Error::Platform(format!(
                        "{error}; restoring the original device class also failed: {restore}"
                    ))),
                };
            }
        };
        set.register_interface(&device).map_err(|error| {
            Error::Platform(format!(
                "WinUSB was installed on {instance}, but interface registration failed: {error}",
            ))
        })?;
        set.restart(&mut device).map_err(|error| Error::Platform(format!(
            "WinUSB was installed on {instance}, but restarting it failed: {error}; reconnect the USB cable",
        )))?;
        Ok(format!(
            "WinUSB installed on {instance} using the current process token. {}",
            if reboot.as_bool() {
                "Windows requests a reboot; restart before pairing."
            } else {
                "Refresh USB controllers, then read addresses and pair."
            }
        ))
    }

    fn set_class(&self, device: &mut SP_DEVINFO_DATA, class: windows::core::GUID) -> Result<()> {
        let value = format!("{{{class:?}}}")
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        // SAFETY: device belongs to this set and value is a terminated UTF-16 GUID.
        unsafe { SetupDiSetDeviceRegistryPropertyW(self.0, device, SPDRP_CLASSGUID, Some(&value)) }
            .map_err(|error| Self::error("set DS3 setup class for driver selection", error))
    }

    fn install(&self, device: &mut SP_DEVINFO_DATA) -> Result<BOOL> {
        let driver = self.winusb_driver(device)?;
        let mut parameters = SP_DEVINSTALL_PARAMS_W {
            cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
            ..Default::default()
        };
        // SAFETY: valid selected device and correctly sized writable parameters.
        unsafe { SetupDiGetDeviceInstallParamsW(self.0, Some(device), &mut parameters) }
            .map_err(|error| Self::error("read selected device installation settings", error))?;
        parameters.Flags |= DI_QUIETINSTALL;
        // SAFETY: this changes only in-memory options for our device information set.
        unsafe { SetupDiSetDeviceInstallParamsW(self.0, Some(device), &parameters) }
            .map_err(|error| Self::error("disable driver installation dialogs", error))?;
        let mut reboot = BOOL::default();
        // SAFETY: both the device and selected driver belong to this live set.
        // An explicit driver bypasses ranking, not Windows access checks.
        unsafe {
            DiInstallDevice(
                None,
                self.0,
                device,
                Some(&driver),
                DIIDFLAG_NOFINISHINSTALLUI,
                Some(&mut reboot),
            )
        }
        .map_err(|error| Self::error("DiInstallDevice(WinUSB)", error))?;
        Ok(reboot)
    }

    fn usb_parent(&self, path: &str) -> Result<String> {
        if path.contains('\0') {
            return Err(Error::InvalidPacket("USB device path contains NUL"));
        }
        let path = path.encode_utf16().chain([0]).collect::<Vec<_>>();
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: path is terminated; interface has the required cbSize.
        unsafe {
            SetupDiOpenDeviceInterfaceW(self.0, PCWSTR(path.as_ptr()), 0, Some(&mut interface))
        }
        .map_err(|error| Self::error("resolve selected HID interface", error))?;
        let mut required = 0;
        // SAFETY: size query only; all optional output pointers are valid.
        let query = unsafe {
            SetupDiGetDeviceInterfaceDetailW(self.0, &interface, None, 0, Some(&mut required), None)
        };
        if let Err(error) = query
            && error.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult()
        {
            return Err(Self::error("query HID interface details", error));
        }
        let mut storage = vec![
            0u64;
            (required as usize)
                .max(size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>())
                .div_ceil(8)
        ];
        let detail = storage
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        let mut device = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: storage has sufficient size/alignment for the variable-length
        // structure and remains live throughout this call.
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(
                self.0,
                &interface,
                Some(detail),
                required,
                None,
                Some(&mut device),
            )
        }
        .map_err(|error| Self::error("read HID interface details", error))?;
        let mut node = device.DevInst;
        for _ in 0..8 {
            let mut id = [0u16; 200];
            // SAFETY: id is writable and node was supplied by Configuration Manager.
            let status = unsafe { CM_Get_Device_IDW(node, &mut id, 0) };
            if status != CR_SUCCESS {
                return Err(Error::Platform(format!(
                    "CM_Get_Device_IDW failed: {status:?}"
                )));
            }
            let id = String::from_utf16_lossy(
                &id[..id.iter().position(|c| *c == 0).unwrap_or(id.len())],
            );
            if Self::is_ds3_usb_instance(&id) {
                return Ok(id);
            }
            let mut parent = 0;
            // SAFETY: parent points to a live output and node is a valid devnode.
            let status = unsafe { CM_Get_Parent(&mut parent, node, 0) };
            if status != CR_SUCCESS {
                break;
            }
            node = parent;
        }
        Err(Error::Unsupported(
            "selected HID interface is not a USB DualShock 3 (054c:0268)",
        ))
    }

    fn is_ds3_usb_instance(instance: &str) -> bool {
        let mut parts = instance.split('\\');
        parts
            .next()
            .is_some_and(|bus| bus.eq_ignore_ascii_case("USB"))
            && parts
                .next()
                .is_some_and(|id| id.eq_ignore_ascii_case("VID_054C&PID_0268"))
            && parts.next().is_some_and(|serial| !serial.is_empty())
            && parts.next().is_none()
    }

    fn winusb_driver(&self, device: &mut SP_DEVINFO_DATA) -> Result<SP_DRVINFO_DATA_V2_W> {
        let mut directory = [0u16; 260];
        // SAFETY: directory is a writable bounded buffer.
        let length = unsafe { GetWindowsDirectoryW(Some(&mut directory)) } as usize;
        if length == 0 || length >= directory.len() {
            return Err(Error::Platform("cannot locate Windows directory".into()));
        }
        let inf = directory[..length]
            .iter()
            .copied()
            .chain("\\INF\\winusb.inf".encode_utf16())
            .chain([0])
            .collect::<Vec<_>>();
        let mut parameters = SP_DEVINSTALL_PARAMS_W {
            cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
            ..Default::default()
        };
        if inf.len() > parameters.DriverPath.len() {
            return Err(Error::Platform("WinUSB INF path is too long".into()));
        }
        // SAFETY: parameters has its required size; device selects its own driver list.
        unsafe { SetupDiGetDeviceInstallParamsW(self.0, Some(device), &mut parameters) }
            .map_err(|error| Self::error("read driver list settings", error))?;
        parameters.Flags |= DI_ENUMSINGLEINF | DI_QUIETINSTALL;
        parameters.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
        parameters.DriverPath.fill(0);
        parameters.DriverPath[..inf.len()].copy_from_slice(&inf);
        // SAFETY: parameters is initialized and its path is NUL-terminated.
        (|| -> windows::core::Result<()> {
            unsafe {
                SetupDiSetDeviceInstallParamsW(self.0, Some(device), &parameters)?;
                SetupDiBuildDriverInfoList(self.0, Some(device), SPDIT_CLASSDRIVER)
            }
        })()
        .map_err(|error| Self::error("enumerate inbox WinUSB driver", error))?;
        let mut sections = Vec::new();
        for index in 0..32 {
            let mut driver = SP_DRVINFO_DATA_V2_W {
                cbSize: size_of::<SP_DRVINFO_DATA_V2_W>() as u32,
                ..Default::default()
            };
            // SAFETY: driver is correctly sized; list belongs to this set.
            if let Err(error) = unsafe {
                SetupDiEnumDriverInfoW(self.0, Some(device), SPDIT_CLASSDRIVER, index, &mut driver)
            } {
                if error.code() == ERROR_NO_MORE_ITEMS.to_hresult() {
                    break;
                }
                return Err(Self::error("enumerate WinUSB driver entry", error));
            }
            let mut detail = SP_DRVINFO_DETAIL_DATA_W {
                cbSize: size_of::<SP_DRVINFO_DETAIL_DATA_W>() as u32,
                ..Default::default()
            };
            // SAFETY: fixed structure is enough for SectionName; Windows fills
            // these fixed fields even when the trailing hardware-ID list is short.
            let result = unsafe {
                SetupDiGetDriverInfoDetailW(
                    self.0,
                    Some(device),
                    &driver,
                    Some(&mut detail),
                    size_of::<SP_DRVINFO_DETAIL_DATA_W>() as u32,
                    None,
                )
            };
            if let Err(error) = result
                && error.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult()
            {
                return Err(Self::error("inspect WinUSB INF section", error));
            }
            let section = String::from_utf16_lossy(
                &detail.SectionName[..detail
                    .SectionName
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(detail.SectionName.len())],
            );
            if section.eq_ignore_ascii_case("WINUSB") {
                return Ok(driver);
            }
            sections.push(section);
        }
        Err(Error::Platform(format!(
            "inbox winusb.inf has no WINUSB installation entry; found {sections:?}"
        )))
    }

    fn register_interface(&self, device: &SP_DEVINFO_DATA) -> Result<()> {
        // A plain inbox WINUSB install does not supply DeviceInterfaceGUIDs.
        // Register an application GUID so nusb can open this exact device.
        let value = "{68284E2A-293C-4D58-9F5B-69A92B11E7D3}"
            .encode_utf16()
            .chain([0, 0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        // SAFETY: the device belongs to this set; registry handle is closed below.
        let key = unsafe {
            SetupDiOpenDevRegKey(
                self.0,
                device,
                DICS_FLAG_GLOBAL.0,
                0,
                DIREG_DEV,
                (KEY_SET_VALUE | KEY_QUERY_VALUE).0,
            )
        }
        .map_err(|error| Self::error("open DS3 device registry key", error))?;
        let mut existing_size = 0;
        // SAFETY: query only the value length; the output pointer is valid.
        let existing = unsafe {
            RegQueryValueExW(
                key,
                w!("DeviceInterfaceGUIDs"),
                None,
                None,
                None,
                Some(&mut existing_size),
            )
        };
        if existing.is_ok() && existing_size > 4 {
            // Existing interface GUIDs are also used by WinUSB; preserve them.
            unsafe {
                let _ = RegCloseKey(key);
            }
            return Ok(());
        }
        // SAFETY: valid handle, terminated constant name, and encoded REG_MULTI_SZ.
        let result = unsafe {
            RegSetValueExW(
                key,
                w!("DeviceInterfaceGUIDs"),
                None,
                REG_MULTI_SZ,
                Some(&value),
            )
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        result
            .ok()
            .map_err(|error| Self::error("register DS3 WinUSB interface GUID", error))
    }

    fn restart(&self, device: &mut SP_DEVINFO_DATA) -> Result<()> {
        let parameters = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: DICS_PROPCHANGE,
            Scope: DICS_FLAG_GLOBAL,
            HwProfile: 0,
        };
        // SAFETY: header is the first field of the repr(C) property-change
        // structure; its full size and live device pointers are supplied.
        (|| -> windows::core::Result<()> {
            unsafe {
                SetupDiSetClassInstallParamsW(
                    self.0,
                    Some(device),
                    Some(&parameters.ClassInstallHeader),
                    size_of::<SP_PROPCHANGE_PARAMS>() as u32,
                )?;
                SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, self.0, Some(device))
            }
        })()
        .map_err(|error| Self::error("restart selected DS3", error))
    }

    fn error(operation: &str, error: windows::core::Error) -> Error {
        Error::Platform(format!(
            "{operation} failed: {error} (HRESULT 0x{:08X}). Attempt used the current process token; no elevation was requested.",
            error.code().0 as u32
        ))
    }
}
