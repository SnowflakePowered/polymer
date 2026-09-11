use super::*;
use crate::{
    TransportKind,
    cable_pairing::CablePairing,
    devices::ProfileId,
    store::{ControllerStore, MemoryControllerStore},
};
use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use bytes::Bytes;
use std::{ffi::CString, sync::Arc};

const CONTROLLER: DeviceAddr = DeviceAddr::public([0x98, 0xe2, 0x55, 0xc2, 0x16, 0x88]);
const HOST: CablePairingHost = CablePairingHost {
    address: DeviceAddr::public([0x48, 0xf1, 0xeb, 0x3a, 0xeb, 0x81]),
    adapter_index: Some(2),
};

#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    Challenge,
    Readback,
    TruncatedMemory,
}

#[derive(Default)]
struct Firmware {
    fault: Fault,
    stage: u8,
    finalized: bool,
    key: [u8; 16],
    host_wire: [u8; 6],
}

impl Firmware {
    fn session(fault: Fault) -> Session<Self> {
        Session::new(
            CableDeviceIdentity {
                profile_id: ProfileId::new("nintendo.switch2.nso-gamecube"),
                vendor_id: 0x057e,
                product_id: 0x2073,
                name: "GameCube fixture".into(),
                serial: None,
                path: CString::new("fixture").unwrap(),
                usb: None,
                #[cfg(windows)]
                dshidmini: None,
            },
            Self {
                fault,
                ..Self::default()
            },
        )
    }
}

#[async_trait]
impl Switch2Commands for Firmware {
    async fn command(&mut self, command: u8, subcommand: u8, data: &[u8]) -> Result<Bytes> {
        match (command, subcommand) {
            (0x15, 1) => {
                if !data.is_empty() {
                    assert_eq!(self.stage, 0);
                    assert_eq!(&data[..2], &[0, 2]);
                    assert_eq!(&data[2..8], &[0x81, 0xeb, 0x3a, 0xeb, 0xf1, 0x48]);
                    assert_eq!(&data[2..8], &data[8..14]);
                    self.host_wire.copy_from_slice(&data[2..8]);
                    self.stage = 1;
                }
                Ok(Bytes::from_static(&[
                    1, 4, 1, 0x88, 0x16, 0xc2, 0x55, 0xe2, 0x98,
                ]))
            }
            (0x15, 4) => {
                assert_eq!(self.stage, 1);
                assert_eq!(data.len(), 17);
                self.stage = 2;
                let mut response = vec![1];
                response.extend(0u8..16);
                for i in 0..16 {
                    self.key[15 - i] = data[i + 1] ^ response[i + 1];
                }
                Ok(response.into())
            }
            (0x15, 2) => {
                assert_eq!(self.stage, 2);
                self.stage = 3;
                let mut challenge: [u8; 16] = data[1..].try_into().unwrap();
                challenge.reverse();
                let mut block = GenericArray::clone_from_slice(&challenge);
                Aes128::new(GenericArray::from_slice(&self.key)).encrypt_block(&mut block);
                let mut response = vec![1];
                response.extend_from_slice(&block);
                if matches!(self.fault, Fault::Challenge) {
                    response[1] ^= 1;
                }
                Ok(response.into())
            }
            (0x15, 3) => {
                assert_eq!(self.stage, 3);
                assert_eq!(data, &[0]);
                self.finalized = true;
                Ok(Bytes::from_static(&[1]))
            }
            (0x02, 4) => {
                assert_eq!(data, &[0x40, 0x7e, 0, 0, 0, 0xa0, 0x1f, 0]);
                if matches!(self.fault, Fault::TruncatedMemory) {
                    return Ok(Bytes::from_static(&[0x40]));
                }
                let mut response = vec![0; 72];
                response[0] = 0x40;
                response[4..8].copy_from_slice(&data[4..8]);
                if self.finalized {
                    response[8] = 2;
                    let mut address = self.host_wire;
                    address.reverse();
                    if matches!(self.fault, Fault::Readback) {
                        address[0] ^= 1;
                    }
                    response[16..22].copy_from_slice(&address);
                    response[56..62].copy_from_slice(&address);
                }
                Ok(response.into())
            }
            _ => panic!("unexpected command {command:02x}/{subcommand:02x}"),
        }
    }
}

#[tokio::test]
async fn usb_handshake_reads_back_host_and_registers_ble_adapter_without_bredr_key() {
    let store = Arc::new(MemoryControllerStore::default());
    let coordinator = CablePairing::new(store.clone());
    let mut session = Firmware::session(Fault::None);
    let verified = coordinator.pair(&mut session, &HOST).await.unwrap();
    assert_eq!(verified.state.controller_address, CONTROLLER);
    assert_eq!(verified.state.host_address, Some(HOST.address));
    let records = store.records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].transport, TransportKind::Ble);
    assert_eq!(records[0].adapter_index, Some(2));
    assert!(records[0].auto_reconnect);
    assert!(records[0].link_key.is_none());
    assert!(records[0].link_key_type.is_none());
}

#[tokio::test]
async fn failed_challenge_never_finalizes_and_failed_readback_never_registers() {
    for fault in [Fault::Challenge, Fault::Readback, Fault::TruncatedMemory] {
        let store = Arc::new(MemoryControllerStore::default());
        let coordinator = CablePairing::new(store.clone());
        let mut session = Firmware::session(fault);
        assert!(coordinator.pair(&mut session, &HOST).await.is_err());
        assert!(store.records().unwrap().is_empty());
        assert_eq!(session.commands.finalized, matches!(fault, Fault::Readback));
    }
}

#[tokio::test]
async fn dedicated_radio_is_rejected_before_switch2_handshake() {
    let coordinator = CablePairing::new(Arc::new(MemoryControllerStore::default()));
    let mut session = Firmware::session(Fault::None);
    let host = CablePairingHost {
        adapter_index: None,
        ..HOST
    };
    assert!(coordinator.pair(&mut session, &host).await.is_err());
    assert_eq!(session.commands.stage, 0);
}

#[test]
fn aes_confirmation_uses_reversed_key_and_challenge_but_ciphertext_order() {
    // NIST AES-128 vector, expressed in Nintendo's key/challenge wire order.
    let key = [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    let challenge = [
        0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0,
    ];
    assert_eq!(
        Firmware::pairing_confirmation(key, challenge),
        [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
            0xc5, 0x5a
        ]
    );
}
