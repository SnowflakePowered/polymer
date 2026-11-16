use std::{io, mem::size_of};
use windows::{
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, Properties::*},
        Foundation::{DEVPROPKEY, ERROR_NO_MORE_ITEMS, ERROR_NOT_FOUND},
    },
    core::{GUID, PCWSTR},
};

use crate::ipc::Connection;

const INTERFACE: GUID = GUID::from_u128(0x16f3fe42_b710_4f67_b6ee_9a8d249c9ce5);
const ADDRESS: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a),
    pid: 1,
};
const HOST: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0xa92f26ca_eda7_4b1d_9db2_27b68aa5a2eb),
    pid: 1,
};
const READ_STATUS: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x3fecf510_cc94_4fbe_8839_738201f84d59),
    pid: 5,
};
const SLOT: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x3fecf510_cc94_4fbe_8839_738201f84d59),
    pid: 6,
};

#[derive(Clone, Debug)]
pub struct Device {
    pub instance_id: String,
    pub address: [u8; 6],
    slot: Option<u32>,
    version: String,
}

#[derive(Clone, Copy, Debug)]
pub struct PairingState {
    pub controller_address: [u8; 6],
    pub host_address: Option<[u8; 6]>,
}

impl Device {
    pub fn enumerate() -> io::Result<Vec<Self>> {
        Ok(DeviceSet::open()?
            .devices()?
            .into_iter()
            .filter(|device| device.is_usb_ds3())
            .collect())
    }

    fn is_usb_ds3(&self) -> bool {
        self.instance_id
            .to_ascii_uppercase()
            .starts_with("USB\\VID_054C&PID_0268\\")
    }

    pub fn read_pairing(&self) -> io::Result<PairingState> {
        let set = DeviceSet::open()?;
        let data = set.selected(self)?;
        let status = set.property(&data, &READ_STATUS, DEVPROP_TYPE_NTSTATUS)?;
        if status.as_slice() != [0, 0, 0, 0] {
            return Err(io::Error::other(
                "DsHidMini has no successful host-address readback",
            ));
        }
        let bytes: [u8; 8] = set
            .property(&data, &HOST, DEVPROP_TYPE_UINT64)?
            .try_into()
            .map_err(|_| io::Error::other("invalid DsHidMini host-address property"))?;
        let numeric = u64::from_le_bytes(bytes);
        if numeric >> 48 != 0 {
            return Err(io::Error::other("invalid DsHidMini host address"));
        }
        let host: [u8; 6] = numeric.to_be_bytes()[2..].try_into().unwrap();
        Ok(PairingState {
            controller_address: self.address,
            host_address: (host != [0; 6]).then_some(host),
        })
    }

    pub fn pair(&self, host: [u8; 6]) -> io::Result<PairingState> {
        if !self.is_usb_ds3() || host == [0; 6] || host == [255; 6] {
            return Err(io::Error::other("invalid DsHidMini USB pairing target"));
        }
        let ipc = Connection::open()?;
        let _guard = ipc.lock()?;
        let set = DeviceSet::open()?;
        set.selected(self)?;
        let devices = set.devices()?;
        let current = devices
            .iter()
            .find(|device| device.instance_id == self.instance_id && device.address == self.address)
            .ok_or_else(|| io::Error::other("DsHidMini controller disconnected; refresh"))?;
        let slot = match current.slot {
            Some(slot @ 1..=255) => slot,
            None if devices.len() == 1 && current.version == "3.3.1187.0" => ipc.legacy_slot()?,
            _ => {
                return Err(io::Error::other(
                    "DsHidMini cannot safely resolve this controller's IPC slot; use a driver exposing IpcSlotIndex, or connect only one controller with driver 3.3.1187.0",
                ));
            }
        };
        ipc.pair(slot, host)?;
        let state = self.read_pairing()?;
        if state.host_address != Some(host) {
            return Err(io::Error::other(
                "DsHidMini host-address readback did not match; pairing was not verified",
            ));
        }
        Ok(state)
    }

    /// Detect HID children of DsHidMini so they aren't offered as raw F2/F5 devices.
    pub fn manages_hid(path: &std::ffi::CStr) -> io::Result<bool> {
        let set = DeviceSet::open()?;
        let devices = set.devices()?;
        if devices.is_empty() {
            return Ok(false);
        }
        let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain([0]).collect();
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: live set, terminated path and initialized output structure.
        unsafe {
            SetupDiOpenDeviceInterfaceW(set.0, PCWSTR(wide.as_ptr()), 0, Some(&mut interface))
        }
        .map_err(io::Error::other)?;
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // The sizing call fills DeviceInfoData even without a detail buffer.
        let result = unsafe {
            SetupDiGetDeviceInterfaceDetailW(set.0, &interface, None, 0, None, Some(&mut data))
        };
        if let Err(error) = result
            && error.code() != windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER.to_hresult()
        {
            return Err(io::Error::other(error));
        }
        let mut node = data.DevInst;
        for _ in 0..8 {
            let mut id = [0u16; 200];
            // SAFETY: Configuration Manager receives bounded storage and a live devnode.
            if unsafe { CM_Get_Device_IDW(node, &mut id, 0) } != CR_SUCCESS {
                break;
            }
            let id = String::from_utf16_lossy(
                &id[..id.iter().position(|c| *c == 0).unwrap_or(id.len())],
            );
            if devices
                .iter()
                .any(|device| device.instance_id.eq_ignore_ascii_case(&id))
            {
                return Ok(true);
            }
            let mut parent = 0;
            if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
                break;
            }
            node = parent;
        }
        Ok(false)
    }
}

struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: uniquely owned SetupAPI information set.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

impl DeviceSet {
    fn open() -> io::Result<Self> {
        // SAFETY: constant interface GUID; no borrowed window handle.
        unsafe {
            SetupDiGetClassDevsW(
                Some(&INTERFACE),
                PCWSTR::null(),
                None,
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        }
        .map(Self)
        .map_err(io::Error::other)
    }

    fn entries(&self) -> io::Result<Vec<SP_DEVINFO_DATA>> {
        let mut devices = Vec::new();
        for index in 0..256 {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: correctly sized output and live set.
            match unsafe { SetupDiEnumDeviceInfo(self.0, index, &mut data) } {
                Ok(()) => devices.push(data),
                Err(error) if error.code() == ERROR_NO_MORE_ITEMS.to_hresult() => {
                    return Ok(devices);
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        Err(io::Error::other("too many DsHidMini devices"))
    }

    fn devices(&self) -> io::Result<Vec<Device>> {
        self.entries()?
            .iter()
            .map(|data| self.identity(data))
            .collect()
    }

    fn identity(&self, data: &SP_DEVINFO_DATA) -> io::Result<Device> {
        let text = self.string(data, &ADDRESS)?;
        if text.len() != 12 || !text.is_ascii() {
            return Err(io::Error::other("invalid DsHidMini controller address"));
        }
        let mut address = [0; 6];
        for (index, byte) in address.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map_err(io::Error::other)?;
        }
        if address == [0; 6] || address == [255; 6] {
            return Err(io::Error::other("invalid DsHidMini controller identity"));
        }
        let slot = match self.property(data, &SLOT, DEVPROP_TYPE_UINT32) {
            Ok(bytes) => Some(u32::from_le_bytes(
                bytes
                    .try_into()
                    .map_err(|_| io::Error::other("invalid DsHidMini slot"))?,
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Device {
            instance_id: self.string(data, &DEVPKEY_Device_InstanceId)?,
            address,
            slot,
            version: self.string(data, &DEVPKEY_Device_DriverVersion)?,
        })
    }

    fn selected(&self, expected: &Device) -> io::Result<SP_DEVINFO_DATA> {
        for data in self.entries()? {
            let found = self.identity(&data)?;
            if found.instance_id == expected.instance_id && found.address == expected.address {
                return Ok(data);
            }
        }
        Err(io::Error::other(
            "selected DsHidMini controller disconnected or changed; refresh",
        ))
    }

    fn string(&self, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> io::Result<String> {
        let bytes = self.property(data, key, DEVPROP_TYPE_STRING)?;
        if bytes.len() % 2 != 0 {
            return Err(io::Error::other("invalid UTF-16 device property"));
        }
        let wide: Vec<_> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect();
        String::from_utf16(&wide).map_err(io::Error::other)
    }

    fn property(
        &self,
        data: &SP_DEVINFO_DATA,
        key: &DEVPROPKEY,
        expected: DEVPROPTYPE,
    ) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0; 4096];
        let mut length = 0;
        let mut kind = DEVPROPTYPE::default();
        // SAFETY: all pointers refer to initialized live structures/bounded buffers.
        unsafe {
            SetupDiGetDevicePropertyW(
                self.0,
                data,
                key,
                &mut kind,
                Some(&mut buffer),
                Some(&mut length),
                0,
            )
        }
        .map_err(|error| {
            if error.code() == ERROR_NOT_FOUND.to_hresult() {
                io::Error::new(io::ErrorKind::NotFound, error)
            } else {
                io::Error::other(error)
            }
        })?;
        if kind != expected || length as usize > buffer.len() {
            return Err(io::Error::other(
                "unexpected DsHidMini device property type",
            ));
        }
        buffer.truncate(length as usize);
        Ok(buffer)
    }
}
