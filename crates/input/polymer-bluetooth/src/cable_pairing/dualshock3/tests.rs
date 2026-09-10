use std::collections::VecDeque;

use super::*;

const PAD: DeviceAddr = DeviceAddr::public([0x00, 0x1b, 0xfb, 0x31, 0x42, 0x53]);
const HOST: CablePairingHost = CablePairingHost {
    address: DeviceAddr::public([0x10, 0x21, 0x32, 0x43, 0x54, 0x65]),
};

enum Step {
    Get {
        id: u8,
        size: usize,
        response: Vec<u8>,
    },
    Set {
        bytes: Vec<u8>,
        fail: bool,
    },
}

impl Step {
    fn addresses(controller: DeviceAddr, host: Option<DeviceAddr>) -> [Self; 2] {
        let mut device = vec![0; 18];
        device[0] = 0xf2;
        device[4..10].copy_from_slice(&controller.bytes());
        let mut central = vec![0; 8];
        central[0] = 0xf5;
        central[2..8].copy_from_slice(&host.map_or([0; 6], DeviceAddr::bytes));
        [
            Self::Get {
                id: 0xf2,
                size: 18,
                response: device,
            },
            Self::Get {
                id: 0xf5,
                size: 8,
                response: central,
            },
        ]
    }
}

struct Script(VecDeque<Step>);

impl UsbFeatureReports for Script {
    fn get_feature(&mut self, report: &mut [u8]) -> Result<usize> {
        let Some(Step::Get { id, size, response }) = self.0.pop_front() else {
            panic!("unexpected GET_FEATURE");
        };
        assert_eq!(report.len(), size);
        assert_eq!(report[0], id);
        assert!(report[1..].iter().all(|byte| *byte == 0));
        report[..response.len()].copy_from_slice(&response);
        Ok(response.len())
    }

    fn set_feature(&mut self, report: &[u8]) -> Result<()> {
        let Some(Step::Set { bytes, fail }) = self.0.pop_front() else {
            panic!("unexpected SET_FEATURE");
        };
        assert_eq!(report, bytes);
        if fail {
            Err(Error::Platform("USB disconnected during write".into()))
        } else {
            Ok(())
        }
    }
}

#[test]
fn cable_handshake_uses_network_order_and_verifies_both_addresses() {
    let old_host = DeviceAddr::public([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, 0xf5]);
    let mut steps = VecDeque::from(Step::addresses(PAD, Some(old_host)));
    steps.push_back(Step::Set {
        bytes: vec![0xf5, 0x01, 0x10, 0x21, 0x32, 0x43, 0x54, 0x65],
        fail: false,
    });
    steps.extend(Step::addresses(PAD, Some(HOST.address)));
    let mut protocol = DualShock3Pairing {
        device: Script(steps),
    };
    let verified = protocol.pair(&HOST).unwrap();
    assert_eq!(verified.state.controller_address, PAD);
    assert_eq!(verified.state.host_address, Some(HOST.address));
    assert!(verified.link_key.is_none());
    assert!(protocol.device.0.is_empty());
}

#[test]
fn repeating_cable_pairing_only_reads_and_preserves_the_wireless_host() {
    let mut steps = VecDeque::from(Step::addresses(PAD, Some(HOST.address)));
    steps.extend(Step::addresses(PAD, Some(HOST.address)));
    let mut protocol = DualShock3Pairing {
        device: Script(steps),
    };
    protocol.pair(&HOST).unwrap();
    assert!(protocol.device.0.is_empty());
}

#[test]
fn failed_write_or_mismatched_readback_never_returns_verified_pairing() {
    for (fail_write, returned_pad, returned_host) in [
        (true, PAD, Some(HOST.address)),
        (false, PAD, None),
        (
            false,
            DeviceAddr::public([1, 2, 3, 4, 5, 6]),
            Some(HOST.address),
        ),
    ] {
        let mut steps = VecDeque::from(Step::addresses(PAD, None));
        steps.push_back(Step::Set {
            bytes: vec![0xf5, 0x01, 0x10, 0x21, 0x32, 0x43, 0x54, 0x65],
            fail: fail_write,
        });
        if !fail_write {
            steps.extend(Step::addresses(returned_pad, returned_host));
        }
        let mut protocol = DualShock3Pairing {
            device: Script(steps),
        };
        assert!(protocol.pair(&HOST).is_err());
        assert!(protocol.device.0.is_empty());
    }
}

#[test]
fn malformed_address_reports_fail_before_any_write() {
    for response in [
        vec![0xf2; 9],
        vec![0; 18],
        vec![0xf2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ] {
        let steps = VecDeque::from([Step::Get {
            id: 0xf2,
            size: 18,
            response,
        }]);
        let mut protocol = DualShock3Pairing {
            device: Script(steps),
        };
        assert!(protocol.pair(&HOST).is_err());
        assert!(protocol.device.0.is_empty());
    }
    for response in [vec![0xf5; 7], vec![]] {
        let mut steps = VecDeque::from(Step::addresses(PAD, None));
        steps.pop_back();
        steps.push_back(Step::Get {
            id: 0xf5,
            size: 8,
            response,
        });
        let mut protocol = DualShock3Pairing {
            device: Script(steps),
        };
        assert!(protocol.pair(&HOST).is_err());
        assert!(protocol.device.0.is_empty());
    }
}

#[test]
fn raw_usb_responses_do_not_need_to_echo_the_report_selector() {
    let [mut device, mut host] = Step::addresses(PAD, Some(HOST.address));
    if let Step::Get { response, .. } = &mut device {
        response[0] = 0x01;
        response.truncate(17);
    }
    if let Step::Get { response, .. } = &mut host {
        response[0] = 0x01;
    }
    let mut protocol = DualShock3Pairing {
        device: Script(VecDeque::from([device, host])),
    };
    let state = protocol.read_pairing().unwrap();
    assert_eq!(state.controller_address, PAD);
    assert_eq!(state.host_address, Some(HOST.address));
}
