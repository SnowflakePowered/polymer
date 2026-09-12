use super::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

const HOST: CablePairingHost = CablePairingHost {
    address: DeviceAddr::public([1, 2, 3, 4, 5, 6]),
    adapter_index: None,
};
const PAD: DeviceAddr = DeviceAddr::public([6, 5, 4, 3, 2, 1]);
const KEY: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const HCI_KEY: [u8; 16] = [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];

#[derive(Default)]
struct Device {
    replies: VecDeque<Vec<u8>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    stage: u8,
    saved: bool,
    reject_stage: u8,
    corrupt_storage: bool,
}

impl InterruptReports for Device {
    fn write(&mut self, packet: &[u8]) -> Result<()> {
        self.writes.lock().unwrap().push(packet.to_vec());
        if packet[0] == 0x80 {
            if matches!(packet[1], 2 | 3) {
                self.replies.push_back(vec![0x81, packet[1]]);
            }
            return Ok(());
        }
        assert_eq!(packet.len(), 64);
        assert_eq!(&packet[2..10], &[0, 1, 0x40, 0x40, 0, 1, 0x40, 0x40]);
        let mut reply = vec![0; 15];
        reply[0] = 0x21;
        reply[13] = 0x80;
        reply[14] = packet[10];
        match packet[10] {
            2 => reply.extend_from_slice(&[3, 0x48, 3, 2, 6, 5, 4, 3, 2, 1, 1, 1]),
            1 => {
                let stage = packet[11];
                assert_eq!(stage, self.stage + 1);
                self.stage = stage;
                if stage == self.reject_stage {
                    reply[13] = 0;
                }
                reply.push(stage);
                match stage {
                    1 => {
                        assert_eq!(&packet[12..18], &[6, 5, 4, 3, 2, 1]);
                        reply.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
                    }
                    2 => reply.extend(KEY.map(|v| v ^ 0xaa)),
                    3 => self.saved = true,
                    _ => unreachable!(),
                }
            }
            0x10 => {
                let address = u32::from_le_bytes(packet[11..15].try_into().unwrap());
                assert_eq!(packet[15], 26);
                reply.extend_from_slice(&packet[11..16]);
                // Include an obsolete record before the active entry.
                let mut record = [0; 26];
                record[1] = 0x22;
                if address == 0x2026 && self.saved {
                    record[0] = 0x95;
                    record[4..10].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
                    record[10..].copy_from_slice(&KEY);
                    if self.corrupt_storage {
                        record[10] ^= 1;
                    }
                } else if address > 0x2000 {
                    record.fill(0xff);
                }
                reply.extend_from_slice(&record);
            }
            other => panic!("unexpected command {other}"),
        }
        self.replies.push_back(vec![0x30; 49]); // Input stream must not be mistaken for an ACK.
        self.replies.push_back(reply);
        Ok(())
    }

    fn read(&mut self, report: &mut [u8], _: i32) -> Result<usize> {
        let reply = self.replies.pop_front().ok_or(Error::ChannelClosed)?;
        report[..reply.len()].copy_from_slice(&reply);
        Ok(reply.len())
    }
}

#[test]
fn handshake_pairing_flash_verification_and_usb_release() {
    let device = Device::default();
    let writes = device.writes.clone();
    let mut protocol = Pairing::new(device);
    assert_eq!(protocol.read_pairing().unwrap().host_address, None);
    let verified = protocol.pair(&HOST).unwrap();
    assert_eq!(verified.state.controller_address, PAD);
    assert_eq!(verified.state.host_address, Some(HOST.address));
    assert_eq!(verified.link_key.unwrap().value, HCI_KEY);
    drop(protocol);
    let writes = writes.lock().unwrap();
    assert_eq!(
        &writes[..4],
        &[vec![0x80, 2], vec![0x80, 3], vec![0x80, 2], vec![0x80, 4]]
    );
    assert_eq!(writes.last().unwrap(), &[0x80, 5]);
    for (index, packet) in writes.iter().filter(|p| p[0] == 1).enumerate() {
        assert_eq!(packet[1], (index as u8) & 15);
    }
}

#[test]
fn rejection_never_commits_and_poisoned_session_cannot_consume_late_replies() {
    let device = Device {
        reject_stage: 2,
        ..Default::default()
    };
    let writes = device.writes.clone();
    let mut protocol = Pairing::new(device);
    assert!(protocol.pair(&HOST).is_err());
    let count = writes.lock().unwrap().len();
    assert!(protocol.pair(&HOST).is_err());
    assert_eq!(writes.lock().unwrap().len(), count);
    assert!(
        !writes
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.len() == 64 && p[10..12] == [1, 3])
    );
    drop(protocol);
    assert_eq!(writes.lock().unwrap().last().unwrap(), &[0x80, 5]);
}

#[test]
fn wrong_radio_and_flash_key_mismatch_cannot_claim_verified_pairing() {
    let mut protocol = Pairing::new(Device::default());
    assert!(
        protocol
            .pair(&CablePairingHost {
                adapter_index: Some(0),
                ..HOST
            })
            .is_err()
    );
    let mut protocol = Pairing::new(Device {
        corrupt_storage: true,
        ..Default::default()
    });
    assert!(protocol.pair(&HOST).is_err());
}

#[tokio::test]
async fn controller_generated_key_is_registered_for_bluetooth() {
    use crate::{
        cable_pairing::{CableDeviceIdentity, CablePairing, feature::FeatureSession},
        devices::ProfileId,
        store::{ControllerStore, MemoryControllerStore},
    };
    let store = Arc::new(MemoryControllerStore::default());
    let identity = CableDeviceIdentity {
        profile_id: ProfileId::new("nintendo.switch.pro"),
        vendor_id: 0x057e,
        product_id: 0x2009,
        name: "Pro Controller".into(),
        serial: None,
        path: std::ffi::CString::new("fixture").unwrap(),
        usb: None,
        #[cfg(windows)]
        dshidmini: None,
    };
    let mut session = FeatureSession::new(identity, Pairing::new(Device::default()));
    CablePairing::new(store.clone())
        .pair(&mut session, &HOST)
        .await
        .unwrap();
    let record = store.records().unwrap().remove(0);
    assert_eq!(record.address, PAD);
    assert_eq!(record.link_key, Some(HCI_KEY));
    assert_eq!(record.profile_id.as_str(), "nintendo.switch.pro");
    assert!(record.auto_reconnect);
}
