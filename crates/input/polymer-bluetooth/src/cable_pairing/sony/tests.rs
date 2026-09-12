use super::*;
use crate::cable_pairing::{CableDeviceIdentity, CablePairing, feature::FeatureSession};
use crate::store::{ControllerStore, SqliteControllerStore};
use std::sync::{Arc, Mutex};

const HOST: CablePairingHost = CablePairingHost {
    address: DeviceAddr::public([0x10, 0x21, 0x32, 0x43, 0x54, 0x65]),
    adapter_index: None,
};
const PAD: DeviceAddr = DeviceAddr::public([0xdc, 0xaf, 0x68, 0x8a, 0xa7, 0x5b]);

struct Device {
    report: Vec<u8>,
    model: Model,
    length: usize,
    reject_write: bool,
    wrong_readback: bool,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Default for Device {
    fn default() -> Self {
        Self {
            // Literal little-endian addresses, independent of production encoding.
            model: Model::DualShock4,
            report: vec![
                0x12, 0x5b, 0xa7, 0x8a, 0x68, 0xaf, 0xdc, 8, 0x25, 0, 0, 0, 0, 0, 0, 0,
            ],
            length: 16,
            reject_write: false,
            wrong_readback: false,
            writes: Arc::default(),
        }
    }
}

impl UsbFeatureReports for Device {
    fn get_feature(&mut self, report: &mut [u8]) -> Result<usize> {
        match self.model {
            Model::DualShock4 => {
                assert_eq!(report.len(), 16);
                assert_eq!(report[0], 0x12);
            }
            Model::DualSense => {
                assert_eq!(report.len(), 20);
                assert_eq!(report[0], 0x09);
            }
        }
        report.copy_from_slice(&self.report);
        Ok(self.length)
    }

    fn set_feature(&mut self, report: &[u8]) -> Result<()> {
        match self.model {
            Model::DualShock4 => {
                assert_eq!(report.len(), 23);
                assert_eq!(report[0], 0x13);
            }
            Model::DualSense => {
                assert_eq!(report.len(), 27);
                assert_eq!(report[0], 0x0a);
                assert_eq!(&report[23..], &[0; 4]);
            }
        }
        assert_eq!(&report[1..7], &[0x65, 0x54, 0x43, 0x32, 0x21, 0x10]);
        self.writes.lock().unwrap().push(report.to_vec());
        if self.reject_write {
            return Err(Error::Platform("USB write failed".into()));
        }
        if !self.wrong_readback {
            self.report[10..16].copy_from_slice(&report[1..7]);
        }
        Ok(())
    }
}

impl Device {
    fn dualsense() -> Self {
        let mut device = Self {
            model: Model::DualSense,
            length: 20,
            ..Default::default()
        };
        device.report[0] = 0x09;
        device.report.resize(20, 0);
        device
    }
    fn session(self) -> FeatureSession<SonyPairing<Self>> {
        let model = self.model;
        let (profile, product) = match model {
            Model::DualShock4 => ("sony.dualshock4", 0x09cc),
            Model::DualSense => ("sony.dualsense", 0x0ce6),
        };
        FeatureSession::new(
            CableDeviceIdentity {
                profile_id: crate::devices::ProfileId::new(profile),
                vendor_id: 0x054c,
                product_id: product,
                name: "DS4 fixture".into(),
                serial: None,
                path: std::ffi::CString::new("fixture").unwrap(),
                usb: None,
                #[cfg(windows)]
                dshidmini: None,
            },
            SonyPairing {
                device: self,
                model,
            },
        )
    }
}

#[tokio::test]
async fn usb_key_matches_persisted_bond_after_restart_and_repair() {
    for (device, profile) in [
        (Device::default(), "sony.dualshock4"),
        (Device::dualsense(), "sony.dualsense"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.db");
        let store = Arc::new(SqliteControllerStore::open(&path).unwrap());
        let writes = device.writes.clone();
        let mut session = device.session();
        let coordinator = CablePairing::new(store.clone());
        let first = coordinator.pair(&mut session, &HOST).await.unwrap();
        assert_eq!(first.state.controller_address, PAD);
        // A same-host retry still writes a key: USB cannot read back the old secret.
        let second = coordinator.pair(&mut session, &HOST).await.unwrap();
        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(first.link_key.unwrap().value.as_slice(), &writes[0][7..23]);
        assert_eq!(second.link_key.unwrap().value.as_slice(), &writes[1][7..23]);
        drop(coordinator);
        drop(store);
        let reopened = SqliteControllerStore::open(&path).unwrap();
        let record = reopened.records().unwrap().remove(0);
        assert_eq!(record.address, PAD);
        assert_eq!(record.profile_id.as_str(), profile);
        assert_eq!(record.link_key.unwrap().as_slice(), &writes[1][7..23]);
        assert_eq!(record.link_key_type, Some(0));
        assert!(record.auto_reconnect);
        assert_eq!(record.adapter_index, None);
    }
}

#[test]
fn malformed_identity_and_wrong_radio_fail_before_writing() {
    for (index, value, length) in [(0, 0x81, 16), (0, 0x12, 15), (0, 0x12, 17)] {
        let mut device = Device::default();
        device.report[index] = value;
        device.length = length;
        let mut protocol = SonyPairing {
            device,
            model: Model::DualShock4,
        };
        assert!(protocol.pair(&HOST).is_err());
        assert!(protocol.device.writes.lock().unwrap().is_empty());
    }
    for invalid in [[0; 6], [255; 6]] {
        let mut device = Device::default();
        device.report[1..7].copy_from_slice(&invalid);
        let mut protocol = SonyPairing {
            device,
            model: Model::DualShock4,
        };
        assert!(protocol.pair(&HOST).is_err());
        assert!(protocol.device.writes.lock().unwrap().is_empty());
    }
    let mut protocol = SonyPairing {
        device: Device::default(),
        model: Model::DualShock4,
    };
    assert!(
        protocol
            .pair(&CablePairingHost {
                adapter_index: Some(0),
                ..HOST
            })
            .is_err()
    );
    assert!(protocol.device.writes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_transfer_or_readback_cannot_register_a_bond() {
    for reject_write in [true, false] {
        let store = Arc::new(crate::store::MemoryControllerStore::default());
        let device = Device {
            reject_write,
            wrong_readback: true,
            ..Default::default()
        };
        let mut session = device.session();
        assert!(
            CablePairing::new(store.clone())
                .pair(&mut session, &HOST)
                .await
                .is_err()
        );
        assert!(store.records().unwrap().is_empty());
    }
}

#[test]
fn dualsense_rejects_ds4_sized_and_wrong_id_reads_before_writing() {
    for (length, id) in [(16, 9), (19, 9), (21, 9), (20, 0x12)] {
        let mut device = Device::dualsense();
        device.length = length;
        device.report[0] = id;
        let mut protocol = SonyPairing {
            device,
            model: Model::DualSense,
        };
        assert!(protocol.pair(&HOST).is_err());
        assert!(protocol.device.writes.lock().unwrap().is_empty());
    }
}
