use super::*;
use crate::store::{MemoryControllerStore, SqliteControllerStore};

struct Session {
    identity: CableDeviceIdentity,
    result: Option<Result<VerifiedCablePairing>>,
}

impl Session {
    fn verified(key: Option<LinkKey>) -> Self {
        Self {
            identity: CableDeviceIdentity {
                profile_id: ProfileId::new("sony.dualshock3"),
                vendor_id: 0x054c,
                product_id: 0x0268,
                name: "DS3 fixture".into(),
                serial: None,
                path: CString::new("fixture").unwrap(),
                usb: None,
                #[cfg(windows)]
                dshidmini: None,
            },
            result: Some(Ok(VerifiedCablePairing {
                state: CablePairingState {
                    controller_address: DeviceAddr::public([0, 1, 2, 3, 4, 5]),
                    host_address: Some(DeviceAddr::public([6, 7, 8, 9, 10, 11])),
                },
                link_key: key,
            })),
        }
    }
}

#[async_trait]
impl CablePairingSession for Session {
    fn identity(&self) -> &CableDeviceIdentity {
        &self.identity
    }
    async fn read_pairing(&mut self) -> Result<CablePairingState> {
        unreachable!("coordinator delegates verification to the session")
    }
    async fn pair(&mut self, _: &CablePairingHost) -> Result<VerifiedCablePairing> {
        self.result.take().unwrap()
    }
}

#[tokio::test]
async fn verified_pairing_survives_registry_reopen_and_replaces_stale_bond_keys() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.db");
    let store = Arc::new(SqliteControllerStore::open(&path).unwrap());
    let address = DeviceAddr::public([0, 1, 2, 3, 4, 5]);
    store
        .put(ControllerRecord {
            address,
            transport: TransportKind::BrEdr,
            profile_id: ProfileId::new("old"),
            link_key: Some([0xaa; 16]),
            link_key_type: Some(4),
            adapter_index: None,
            auto_reconnect: false,
            player_index: 2,
        })
        .unwrap();
    let host = CablePairingHost {
        address: DeviceAddr::public([6, 7, 8, 9, 10, 11]),
    };
    let pairing = CablePairing::new(store.clone());
    pairing
        .pair(&mut Session::verified(None), &host)
        .await
        .unwrap();
    drop(pairing);
    drop(store);
    let reopened = SqliteControllerStore::open(&path).unwrap();
    let records = reopened.records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].address, address);
    assert_eq!(records[0].profile_id.as_str(), "sony.dualshock3");
    assert!(records[0].auto_reconnect);
    assert_eq!(records[0].player_index, 2);
    assert_eq!(records[0].link_key, None);
    assert_eq!(records[0].link_key_type, None);
}

#[tokio::test]
async fn coordinator_supports_keyed_protocols_and_does_not_register_failed_handshakes() {
    let store = Arc::new(MemoryControllerStore::default());
    let pairing = CablePairing::new(store.clone());
    let host = CablePairingHost {
        address: DeviceAddr::public([6, 7, 8, 9, 10, 11]),
    };
    let mut session = Session::verified(None);
    session.result = Some(Err(Error::InvalidPacket("readback mismatch")));
    assert!(pairing.pair(&mut session, &host).await.is_err());
    assert!(store.records().unwrap().is_empty());
    let mut session = Session::verified(Some(LinkKey {
        value: [0x42; 16],
        key_type: 4,
    }));
    session.identity.profile_id = ProfileId::new("test.keyed-protocol");
    pairing.pair(&mut session, &host).await.unwrap();
    let record = store.records().unwrap().remove(0);
    assert_eq!(record.profile_id.as_str(), "test.keyed-protocol");
    assert_eq!(record.link_key, Some([0x42; 16]));
    assert_eq!(record.link_key_type, Some(4));
}

#[tokio::test]
async fn invalid_host_is_rejected_before_invoking_the_handshake() {
    let pairing = CablePairing::new(Arc::new(MemoryControllerStore::default()));
    for address in [
        DeviceAddr::public([0; 6]),
        DeviceAddr::public([255; 6]),
        DeviceAddr::new([1, 2, 3, 4, 5, 6], AddressKind::Random),
    ] {
        let mut session = Session::verified(None);
        assert!(
            pairing
                .pair(&mut session, &CablePairingHost { address })
                .await
                .is_err()
        );
        assert!(session.result.is_some());
    }
}

struct FailedStore;

impl ControllerStore for FailedStore {
    fn records(&self) -> Result<Vec<ControllerRecord>> {
        Ok(Vec::new())
    }
    fn put(&self, _: ControllerRecord) -> Result<()> {
        Err(Error::Store("disk full".into()))
    }
    fn remove(&self, _: DeviceAddr) -> Result<()> {
        unreachable!()
    }
}

#[tokio::test]
async fn registration_failure_reports_that_hardware_has_already_been_paired() {
    let pairing = CablePairing::new(Arc::new(FailedStore));
    let host = CablePairingHost {
        address: DeviceAddr::public([6, 7, 8, 9, 10, 11]),
    };
    let error = pairing
        .pair(&mut Session::verified(None), &host)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("is paired to 06:07:08:09:0A:0B"));
    assert!(error.to_string().contains("registration failed"));
}
