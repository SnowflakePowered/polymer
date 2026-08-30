use crate::configuration::SyntheticWiimote;
use parking_lot::RwLock;
use snowglass_input_layout::topology::RawHidBinding;
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
    pub(crate) fn rebind(&self, bindings: &[RawHidBinding]) {
        *self.devices.write() = bindings
            .iter()
            .map(SyntheticWiimote::from_binding)
            .collect();
    }

    pub(crate) unsafe fn extends(&self, interface: *const GUID) -> bool {
        !self.devices.read().is_empty()
            && !interface.is_null()
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

    pub(crate) unsafe fn append(&self, buffer: *mut u16, buffer_len: u32) -> CONFIGRET {
        if buffer.is_null() {
            return CR_BUFFER_SMALL;
        }
        let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, buffer_len as usize) };
        let end = buffer
            .windows(2)
            .position(|pair| pair == [0, 0])
            .or_else(|| buffer.iter().position(|value| *value == 0))
            .unwrap_or(buffer.len());
        let required = end + self.additional_len() as usize + 1;
        if required > buffer.len() {
            return CR_BUFFER_SMALL;
        }
        let mut offset = end;
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
        self.devices
            .read()
            .iter()
            .find(|device| device.path_matches(path))
            .cloned()
    }
}
