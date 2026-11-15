use windows::{
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                CM_Get_DevNode_PropertyW, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_SUCCESS,
                DIUNINSTALLDRIVER_FLAGS, DiUninstallDriverW, SetupGetInfDriverStoreLocationW,
            },
            Properties::{
                DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_DriverInfPath,
                DEVPKEY_Device_DriverProvider, DEVPKEY_Device_FriendlyName, DEVPROP_TYPE_STRING,
                DEVPROPTYPE,
            },
        },
        Foundation::{DEVPROPKEY, ERROR_NOT_FOUND},
    },
    core::{BOOL, HSTRING, PCWSTR},
};

pub(super) struct DeviceNode(u32);

impl DeviceNode {
    pub(super) fn locate(device_id: &str) -> Option<Self> {
        let mut node = 0;
        let id = HSTRING::from(device_id);
        (unsafe { CM_Locate_DevNodeW(&mut node, &id, CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS)
            .then_some(Self(node))
    }

    pub(super) fn name(&self) -> Option<String> {
        self.property(&DEVPKEY_Device_FriendlyName)
            .or_else(|| self.property(&DEVPKEY_Device_DeviceDesc))
    }

    fn property(&self, key: &DEVPROPKEY) -> Option<String> {
        let mut kind = DEVPROPTYPE::default();
        let mut buffer = [0u16; 260];
        let mut size = size_of_val(&buffer) as u32;
        let result = unsafe {
            CM_Get_DevNode_PropertyW(
                self.0,
                key,
                &mut kind,
                Some(buffer.as_mut_ptr().cast()),
                &mut size,
                0,
            )
        };
        let length = (size as usize / 2).min(buffer.len());
        (result == CR_SUCCESS && kind == DEVPROP_TYPE_STRING).then(|| {
            String::from_utf16_lossy(&buffer[..length])
                .trim_end_matches('\0')
                .to_owned()
        })
    }
}

pub(super) struct DriverPackage {
    inf: String,
    provider: String,
}

impl DriverPackage {
    const PROVIDER: &str = "libwdi";

    pub(super) fn of(node: &DeviceNode) -> Option<Self> {
        Some(Self {
            inf: node.property(&DEVPKEY_Device_DriverInfPath)?,
            provider: node.property(&DEVPKEY_Device_DriverProvider)?,
        })
    }

    pub(super) fn removable(&self) -> bool {
        let inf = self.inf.to_ascii_lowercase();
        self.provider == Self::PROVIDER
            && inf.starts_with("oem")
            && inf.ends_with(".inf")
            && !inf.contains(['\\', '/', ':'])
    }

    pub(super) fn uninstall(&self) -> windows::core::Result<bool> {
        if !self.removable() {
            return Err(ERROR_NOT_FOUND.to_hresult().into());
        }
        let mut store = [0u16; 520];
        let mut reboot = BOOL::default();
        unsafe {
            SetupGetInfDriverStoreLocationW(
                &HSTRING::from(&self.inf),
                None,
                PCWSTR::null(),
                &mut store,
                None,
            )?;
            DiUninstallDriverW(
                None,
                PCWSTR(store.as_ptr()),
                DIUNINSTALLDRIVER_FLAGS(0),
                Some(&mut reboot),
            )?;
        }
        Ok(reboot.as_bool())
    }
}
