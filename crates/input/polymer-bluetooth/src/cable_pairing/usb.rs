//! DS3 pairing requests on endpoint zero. No USB input endpoints are opened.

use std::time::Duration;

use nusb::{
    MaybeFuture,
    transfer::{ControlIn, ControlOut, ControlType, Recipient},
};

use super::{CableDeviceIdentity, hid::UsbFeatureReports};
use crate::{Error, Result};

pub(super) struct DualShock3Usb {
    interface: nusb::Interface,
}

impl DualShock3Usb {
    #[cfg(windows)]
    pub async fn enumerate() -> Result<Vec<CableDeviceIdentity>> {
        Ok(nusb::list_devices()
            .await?
            .filter(|info| info.vendor_id() == 0x054c && info.product_id() == 0x0268)
            .filter(|info| {
                info.driver()
                    .is_some_and(|driver| driver.eq_ignore_ascii_case("winusb"))
            })
            .map(|info| CableDeviceIdentity {
                profile_id: crate::devices::ProfileId::new("sony.dualshock3"),
                vendor_id: info.vendor_id(),
                product_id: info.product_id(),
                name: info
                    .product_string()
                    .unwrap_or("DualShock 3 / SIXAXIS")
                    .into(),
                serial: info.serial_number().map(str::to_owned),
                // Only displayed in the UI. Opening uses the original DeviceInfo.
                path: std::ffi::CString::new(info.instance_id().to_string_lossy().as_bytes())
                    .unwrap(),
                usb: Some(info),
            })
            .collect())
    }

    #[cfg(not(windows))]
    pub async fn enumerate() -> Result<Vec<CableDeviceIdentity>> {
        Ok(Vec::new())
    }

    pub async fn open(info: &nusb::DeviceInfo) -> Result<Self> {
        let device = info.open().await?;
        let configuration = device.active_configuration()?;
        let interface_number = configuration
            .interfaces()
            .find_map(|interface| {
                interface
                    .alt_settings()
                    .find(|descriptor| descriptor.class() == 3)
                    .map(|descriptor| descriptor.interface_number())
            })
            .ok_or(Error::Unsupported("DS3 USB pairing interface not found"))?;
        let interface = device.claim_interface(interface_number).await?;
        Ok(Self { interface })
    }
}

impl UsbFeatureReports for DualShock3Usb {
    fn get_feature(&mut self, report: &mut [u8]) -> Result<usize> {
        let id = *report
            .first()
            .ok_or(Error::InvalidPacket("empty DS3 feature request"))?;
        if !matches!((id, report.len()), (0xf2, 18) | (0xf5, 8)) {
            return Err(Error::Unsupported(
                "raw DS3 USB access is limited to pairing reports",
            ));
        }
        let bytes = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: 0x01,
                    value: 0x0300 | u16::from(id),
                    index: u16::from(self.interface.interface_number()),
                    length: report.len() as u16,
                },
                Duration::from_secs(2),
            )
            .wait()
            .map_err(|error| {
                Error::Platform(format!("DS3 USB GET_REPORT 0x{id:02X} failed: {error}",))
            })?;
        report[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    fn set_feature(&mut self, report: &[u8]) -> Result<()> {
        let wire = HostWrite::from_feature(report)?;
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: 0x09,
                    value: 0x03f5,
                    index: u16::from(self.interface.interface_number()),
                    data: &wire.0,
                },
                Duration::from_secs(2),
            )
            .wait()
            .map_err(|error| Error::Platform(format!("DS3 USB SET_REPORT 0xF5 failed: {error}",)))
    }
}

struct HostWrite([u8; 8]);

impl HostWrite {
    fn from_feature(report: &[u8]) -> Result<Self> {
        let [0xf5, 0x01, address @ ..] = report else {
            return Err(Error::Unsupported(
                "raw DS3 USB writes are limited to host provisioning",
            ));
        };
        let address: [u8; 6] = address
            .try_into()
            .map_err(|_| Error::InvalidPacket("DS3 host address must contain six bytes"))?;
        // sixpair's raw USB payload starts 01 00. The report selector F5 is
        // in wValue, unlike the HIDAPI envelope used by the protocol session.
        // Reference: https://www.pabr.org/sixlinux/sixpair.c
        let mut wire = [0x01, 0, 0, 0, 0, 0, 0, 0];
        wire[2..].copy_from_slice(&address);
        Ok(Self(wire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_host_write_uses_usb_payload_instead_of_hidapi_report_prefix() {
        let report =
            HostWrite::from_feature(&[0xf5, 1, 0x10, 0x21, 0x32, 0x43, 0x54, 0x65]).unwrap();
        assert_eq!(report.0, [1, 0, 0x10, 0x21, 0x32, 0x43, 0x54, 0x65]);
        assert!(HostWrite::from_feature(&[0xf4, 0x42, 3, 0, 0]).is_err());
        assert!(HostWrite::from_feature(&[0xf5, 1, 2, 3]).is_err());
    }
}
