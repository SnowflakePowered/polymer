use hidapi::{BusType, HidApi, HidDevice};

use super::CableDeviceIdentity;
use crate::{Error, Result, devices::ProfileId};

/// Only feature transfers used by cable provisioning; never reads USB input.
pub(super) trait UsbFeatureReports {
    fn get_feature(&mut self, report: &mut [u8]) -> Result<usize>;
    fn set_feature(&mut self, report: &[u8]) -> Result<()>;
}

pub(super) struct CableHid(HidDevice);

impl CableHid {
    pub async fn enumerate() -> Result<Vec<CableDeviceIdentity>> {
        tokio::task::spawn_blocking(|| {
            let api = HidApi::new().map_err(Self::error)?;
            let mut devices = Vec::new();
            for device in api.device_list().filter(|device| Self::supported(device)) {
                #[cfg(windows)]
                if device.product_id() == 0x0268
                    && polymer_dshidmini::Device::manages_hid(device.path()).map_err(|error| {
                        Error::Platform(format!("resolve DsHidMini HID parent: {error}"))
                    })?
                {
                    continue;
                }
                let (profile, name) = match device.product_id() {
                    0x0268 => ("sony.dualshock3", "DualShock 3 / SIXAXIS"),
                    0x05c4 | 0x09cc => ("sony.dualshock4", "DualShock 4"),
                    _ => ("sony.dualsense", "DualSense"),
                };
                devices.push(CableDeviceIdentity {
                    profile_id: ProfileId::new(profile),
                    vendor_id: device.vendor_id(),
                    product_id: device.product_id(),
                    name: device.product_string().unwrap_or(name).into(),
                    serial: device.serial_number().map(str::to_owned),
                    path: device.path().to_owned(),
                    usb: None,
                    #[cfg(windows)]
                    dshidmini: None,
                });
            }
            Ok(devices)
        })
        .await
        .map_err(|error| Error::Platform(format!("USB enumeration task failed: {error}")))?
    }

    fn supported(device: &hidapi::DeviceInfo) -> bool {
        // 054c:042f is the Navigation controller, not a DS3.
        matches!(device.bus_type(), BusType::Usb)
            && device.vendor_id() == 0x054c
            && matches!(device.product_id(), 0x0268 | 0x05c4 | 0x09cc | 0x0ce6 | 0x0df2)
            // DS4 v2 also exposes audio; select the gamepad HID collection.
            && (device.product_id() == 0x0268 || (device.usage_page() == 1 && device.usage() == 5))
    }

    pub async fn open(identity: CableDeviceIdentity) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            let api = HidApi::new().map_err(Self::error)?;
            let info = api
                .device_list()
                .find(|device| device.path() == identity.path())
                .ok_or_else(|| {
                    Error::Platform(
                        "USB controller disconnected; refresh the cable device list".into(),
                    )
                })?;
            if !Self::supported(info)
                || info.vendor_id() != identity.vendor_id
                || info.product_id() != identity.product_id
            {
                return Err(Error::Unsupported(
                    "USB interface no longer matches the selected cable controller",
                ));
            }
            info.open_device(&api).map(Self).map_err(Self::error)
        })
        .await
        .map_err(|error| Error::Platform(format!("USB open task failed: {error}")))?
    }

    fn error(error: hidapi::HidError) -> Error {
        Error::Platform(format!("USB pairing HID access failed: {error}"))
    }
}

impl UsbFeatureReports for CableHid {
    fn get_feature(&mut self, report: &mut [u8]) -> Result<usize> {
        let id = report[0];
        let size = report.len();
        self.0.get_feature_report(report).map_err(|error| {
            Error::Platform(format!(
                "USB pairing GET_FEATURE 0x{id:02X} ({size} bytes) failed: {error}. {}",
                if cfg!(windows) && matches!(id, 0xf2 | 0xf5) {
                    "The standard Windows HidUsb driver blocks the DS3's undeclared F2/F5 pairing reports. Use DsHidMini with IPC enabled, or WinUSB, then refresh USB controllers."
                } else { "Check that the controller supports this USB pairing feature report." },
            ))
        })
    }

    fn set_feature(&mut self, report: &[u8]) -> Result<()> {
        self.0.send_feature_report(report).map_err(|error| {
            Error::Platform(format!(
                "USB pairing SET_FEATURE 0x{:02X} ({} bytes) failed: {error}",
                report[0],
                report.len(),
            ))
        })
    }
}
