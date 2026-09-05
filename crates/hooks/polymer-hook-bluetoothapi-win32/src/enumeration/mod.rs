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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

    #[test]
    fn resolves_a_normalized_synthetic_path_by_port() {
        let enumeration = SyntheticEnumeration::default();
        enumeration.rebind(
            "test-session",
            &[RawHidBinding {
                player: 1,
                port: 0,
                vendor_id: 0x057e,
                product_id: 0x0306,
                pipe: r"\\.\pipe\test-session-hid-0".to_owned(),
            }],
        );
        let path = OsStr::new(
            r"\\?\hid#normalized-by-dolphin#polymer-0#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        )
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();

        let device =
            unsafe { enumeration.find_path(path.as_ptr()) }.expect("synthetic path should match");
        assert_eq!(device.port, 0);
        assert!(device.active);
    }

    #[test]
    fn removes_filtered_native_devices_and_appends_polymer_devices() {
        let enumeration = SyntheticEnumeration::default();
        enumeration.rebind("test-session", &[]);
        let mut list = [r"\\?\hid#bluetooth-device", r"\\?\hid#usb-device"]
            .into_iter()
            .flat_map(|path| OsStr::new(path).encode_wide().chain(Some(0)))
            .chain(Some(0))
            .chain(std::iter::repeat_n(
                0,
                enumeration.additional_len() as usize,
            ))
            .collect::<Vec<_>>();

        assert_eq!(
            unsafe {
                enumeration.filter_and_append(list.as_mut_ptr(), list.len() as u32, |path| {
                    !String::from_utf16_lossy(path).contains("bluetooth")
                })
            },
            CR_SUCCESS
        );
        let rendered = String::from_utf16_lossy(&list);
        assert!(!rendered.contains("bluetooth-device"));
        assert!(rendered.contains("usb-device"));
        for port in 0..4 {
            assert!(rendered.contains(&format!("polymer-{port}")));
        }
    }

    #[test]
    fn keeps_inactive_port_paths_stable_for_live_additions() {
        let enumeration = SyntheticEnumeration::default();
        enumeration.rebind("test-session", &[]);
        let path = OsStr::new(
            r"\\?\hid#vid_057e&pid_0306#polymer-3#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        )
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();

        let Some(device) = (unsafe { enumeration.find_path(path.as_ptr()) }) else {
            panic!("inactive synthetic port should remain discoverable");
        };
        assert_eq!(device.port, 3);
        assert!(!device.active);
    }
}
