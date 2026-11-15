use super::{InstallEvent, ProgressPipe, SetupState, bluetooth::BluetoothAdapter};
use libwdi_sys::{
    WDI_ERROR_INVALID_PARAM, WDI_ERROR_NO_DEVICE, WDI_ERROR_RESOURCE, WDI_SUCCESS, WDI_WINUSB,
    wdi_create_list, wdi_destroy_list, wdi_device_info, wdi_install_driver,
    wdi_options_create_list, wdi_options_install_driver, wdi_options_prepare_driver,
    wdi_prepare_driver, wdi_strerror,
};
use std::{
    ffi::{CStr, CString, OsString},
    hash::{BuildHasher, Hasher},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
};
use windows::{
    Win32::{
        Foundation::{ERROR_NOT_FOUND, ERROR_SUCCESS_REBOOT_REQUIRED, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::CreateDirectoryW,
        System::SystemInformation::GetWindowsDirectoryW,
    },
    core::{HRESULT, HSTRING, w},
};

const INF_NAME: &CStr = c"winusb.inf";
const INSTALL_TIMEOUT_MS: u32 = 120_000;

pub(super) struct DeviceList(*mut wdi_device_info);

impl DeviceList {
    pub(super) fn create() -> Result<Self, i32> {
        let mut list = std::ptr::null_mut();
        let mut options = wdi_options_create_list {
            list_all: 1,
            list_hubs: 0,
            trim_whitespaces: 1,
        };
        match unsafe { wdi_create_list(&mut list, &mut options) } {
            WDI_SUCCESS => Ok(Self(list)),
            WDI_ERROR_NO_DEVICE => Ok(Self(std::ptr::null_mut())),
            code => Err(code),
        }
    }

    pub(super) fn devices(&self) -> impl Iterator<Item = *mut wdi_device_info> + '_ {
        std::iter::successors((!self.0.is_null()).then_some(self.0), |device| {
            let next = unsafe { (**device).next };
            (!next.is_null()).then_some(next)
        })
    }
}

impl Drop for DeviceList {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { wdi_destroy_list(self.0) };
        }
    }
}

struct ProtectedDirectory(PathBuf);

impl ProtectedDirectory {
    fn create() -> windows::core::Result<Self> {
        let mut root = [0u16; 260];
        let length = unsafe { GetWindowsDirectoryW(Some(&mut root)) } as usize;
        if length == 0 || length >= root.len() {
            return Err(windows::core::Error::from_thread());
        }
        let suffix = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let path = PathBuf::from(OsString::from_wide(&root[..length]))
            .join("Temp")
            .join(format!("polymer-winusb-{suffix:016x}"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )?;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let created =
            unsafe { CreateDirectoryW(&HSTRING::from(path.as_os_str()), Some(&attributes)) };
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        created.map(|()| Self(path))
    }
}

impl Drop for ProtectedDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) struct WinUsbInstaller;

impl WinUsbInstaller {
    pub(super) fn run(parent: u32, device_id: &str) -> i32 {
        let mut pipe = ProgressPipe::connect(parent);
        match Self::install(device_id, &mut pipe) {
            Ok(()) => WDI_SUCCESS,
            Err(code) => code,
        }
    }

    pub(super) fn outcome(code: u32) -> SetupState {
        match code as i32 {
            WDI_SUCCESS => SetupState::Installed,
            code => SetupState::Failed(format!(
                "The WinUSB driver could not be installed: {}.",
                Self::describe(code)
            )),
        }
    }

    pub(super) fn remove(parent: u32, device_id: &str) -> i32 {
        let mut pipe = ProgressPipe::connect(parent);
        pipe.send(&InstallEvent::Action {
            description: "Giving the adapter back to Windows".to_owned(),
        });
        let removed = BluetoothAdapter::list()
            .unwrap_or_default()
            .into_iter()
            .find(|adapter| adapter.device_id == device_id && adapter.removable())
            .and_then(|adapter| adapter.package)
            .ok_or_else(|| windows::core::Error::from(ERROR_NOT_FOUND.to_hresult()))
            .and_then(|package| package.uninstall());
        match removed {
            Ok(false) => 0,
            Ok(true) => ERROR_SUCCESS_REBOOT_REQUIRED.0 as i32,
            Err(error) => error.code().0,
        }
    }

    pub(super) fn removal_outcome(code: u32) -> SetupState {
        match code {
            0 => SetupState::Installed,
            code if code == ERROR_SUCCESS_REBOOT_REQUIRED.0 => SetupState::RestartRequired,
            code => SetupState::Failed(format!(
                "The adapter could not be given back to Windows: {}",
                windows::core::Error::from(HRESULT(code as i32)).message()
            )),
        }
    }

    pub(super) fn describe(code: i32) -> String {
        unsafe { CStr::from_ptr(wdi_strerror(code)) }
            .to_string_lossy()
            .into_owned()
    }

    fn install(device_id: &str, pipe: &mut ProgressPipe) -> Result<(), i32> {
        let list = DeviceList::create()?;
        let device = list
            .devices()
            .find(|device| {
                BluetoothAdapter::from_device(*device)
                    .is_some_and(|adapter| adapter.device_id == device_id && !adapter.composite)
            })
            .ok_or(WDI_ERROR_NO_DEVICE)?;
        let directory = ProtectedDirectory::create().map_err(|_| WDI_ERROR_RESOURCE)?;
        let path = directory
            .0
            .to_str()
            .and_then(|path| CString::new(path).ok())
            .ok_or(WDI_ERROR_INVALID_PARAM)?;
        pipe.send(&InstallEvent::Action {
            description: "Preparing the WinUSB driver".to_owned(),
        });
        let mut prepare = wdi_options_prepare_driver {
            driver_type: WDI_WINUSB as _,
            vendor_name: std::ptr::null_mut(),
            device_guid: std::ptr::null_mut(),
            disable_cat: 0,
            disable_signing: 0,
            cert_subject: std::ptr::null_mut(),
            use_wcid_driver: 0,
            external_inf: 0,
        };
        match unsafe { wdi_prepare_driver(device, path.as_ptr(), INF_NAME.as_ptr(), &mut prepare) }
        {
            WDI_SUCCESS => {}
            code => return Err(code),
        }
        pipe.send(&InstallEvent::Action {
            description: "Installing the WinUSB driver".to_owned(),
        });
        let mut install = wdi_options_install_driver {
            hWnd: std::ptr::null_mut(),
            install_filter_driver: 0,
            pending_install_timeout: INSTALL_TIMEOUT_MS,
        };
        match unsafe { wdi_install_driver(device, path.as_ptr(), INF_NAME.as_ptr(), &mut install) }
        {
            WDI_SUCCESS => Ok(()),
            code => Err(code),
        }
    }
}
