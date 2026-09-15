use crate::configuration::SyntheticWiimote;
use parking_lot::RwLock;
use polymer_controllers::topology::RawHidBinding;
use windows_sys::{
    Win32::Devices::{
        DeviceAndDriverInstallation::{CONFIGRET, CR_BUFFER_SMALL, CR_SUCCESS},
        HumanInterfaceDevice::GUID_DEVINTERFACE_HID,
    },
    core::GUID,
};

pub(crate) struct SyntheticEnumeration {
    devices: RwLock<Vec<SyntheticWiimote>>,
}

impl Default for SyntheticEnumeration {
    fn default() -> Self {
        Self {
            devices: RwLock::new(Vec::new()),
        }
    }
}

impl SyntheticEnumeration {
    pub(crate) fn rebind(&self, session: &str, bindings: &[RawHidBinding]) {
        *self.devices.write() = (0..4)
            .map(|port| {
                let binding = bindings.iter().find(|binding| binding.port == port);
                SyntheticWiimote::for_port(session, port, binding)
            })
            .collect();
    }

    pub(crate) unsafe fn supports(&self, interface: *const GUID) -> bool {
        !interface.is_null()
            && unsafe {
                let interface = &*interface;
                interface.data1 == GUID_DEVINTERFACE_HID.data1
                    && interface.data2 == GUID_DEVINTERFACE_HID.data2
                    && interface.data3 == GUID_DEVINTERFACE_HID.data3
                    && interface.data4 == GUID_DEVINTERFACE_HID.data4
            }
    }

    pub(crate) fn additional_len(&self) -> u32 {
        self.devices
            .read()
            .iter()
            .map(|device| device.path.len() as u32)
            .sum()
    }

    pub(crate) unsafe fn filter_and_append(
        &self,
        buffer: *mut u16,
        buffer_len: u32,
        mut retain: impl FnMut(&[u16]) -> bool,
    ) -> CONFIGRET {
        if buffer.is_null() {
            return CR_BUFFER_SMALL;
        }
        let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, buffer_len as usize) };
        let mut retained = Vec::<Vec<u16>>::new();
        let mut offset = 0;
        while offset < buffer.len() && buffer[offset] != 0 {
            let Some(length) = buffer[offset..].iter().position(|value| *value == 0) else {
                return CR_BUFFER_SMALL;
            };
            let path = &buffer[offset..offset + length];
            if retain(path) {
                retained.push(path.to_vec());
            }
            offset += length + 1;
        }
        let required = retained.iter().map(|path| path.len() + 1).sum::<usize>()
            + self.additional_len() as usize
            + 1;
        if required > buffer.len() {
            return CR_BUFFER_SMALL;
        }
        buffer.fill(0);
        let mut offset = 0usize;
        for path in retained {
            buffer[offset..offset + path.len()].copy_from_slice(&path);
            offset += path.len() + 1;
        }
        for device in self.devices.read().iter() {
            buffer[offset..offset + device.path.len()].copy_from_slice(&device.path);
            offset += device.path.len();
        }
        buffer[offset] = 0;
        CR_SUCCESS
    }

    pub(crate) unsafe fn find_path(&self, path: *const u16) -> Option<SyntheticWiimote> {
        if path.is_null() {
            return None;
        }
        let length = (0..32_768).find(|offset| unsafe { *path.add(*offset) == 0 })?;
        let path = unsafe { std::slice::from_raw_parts(path, length) };
        let devices = self.devices.read();
        if let Some(device) = devices.iter().find(|device| device.path_matches(path)) {
            return Some(device.clone());
        }
        let path = String::from_utf16_lossy(path).to_ascii_lowercase();
        if let Some(port) = path
            .split("polymer-")
            .nth(1)
            .and_then(|suffix| suffix.split('#').next())
            .and_then(|port| port.parse::<u8>().ok())
            && let Some(device) = devices.iter().find(|device| device.port == port)
        {
            return Some(device.clone());
        }
        None
    }
}
