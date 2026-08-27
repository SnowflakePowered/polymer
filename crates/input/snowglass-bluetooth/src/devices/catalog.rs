use std::{sync::Arc, time::Duration};

use uuid::Uuid;

use super::{
    BondingMode, ConnectionPolicy, ControllerProfile, DeviceProfile, HostIdentity, MatchRule,
    PairingPolicy, ReconnectPolicy,
    nintendo::{
        switch::SwitchDevice,
        switch2::{SERVICE_UUID as SWITCH2_SERVICE_UUID, Switch2Device},
        wiimote::WiimoteDevice,
    },
};

const SONY_VENDOR_ID: u16 = 0x054c;
const NINTENDO_COMPANY_ID: u16 = 0x0553;
const HID_SERVICE: Uuid = Uuid::from_u128(0x0000_1124_0000_1000_8000_0080_5f9b_34fb);
const BLE_HID_SERVICE: Uuid = Uuid::from_u128(0x0000_1812_0000_1000_8000_0080_5f9b_34fb);

pub(super) fn profiles() -> Vec<Arc<dyn ControllerProfile>> {
    let profile =
        |descriptor: DeviceProfile| -> Arc<dyn ControllerProfile> { Arc::new(descriptor) };
    const ENTRIES: [(&str, &str, &[&str]); 7] = [
        (
            "nintendo.switch.pro",
            "Nintendo Switch Pro Controller",
            &["Pro Controller"],
        ),
        (
            "nintendo.switch.joycon-left",
            "Joy-Con (L)",
            &["Joy-Con (L)"],
        ),
        (
            "nintendo.switch.joycon-right",
            "Joy-Con (R)",
            &["Joy-Con (R)"],
        ),
        (
            "nintendo.switch.nso-nes",
            "Nintendo Switch Online NES Controller",
            &["NES Controller", "HVC Controller (L)", "HVC Controller (R)"],
        ),
        (
            "nintendo.switch.nso-snes",
            "Nintendo Switch Online SNES Controller",
            &["SNES Controller"],
        ),
        (
            "nintendo.switch.nso-n64",
            "Nintendo Switch Online N64 Controller",
            &["Nintendo N64 Controller", "N64 Controller"],
        ),
        (
            "nintendo.switch.nso-genesis",
            "Nintendo Switch Online Genesis / Mega Drive Controller",
            &["MD/Gen Control Pad"],
        ),
    ];
    let mut profiles = ENTRIES
        .into_iter()
        .map(|(id, label, names)| {
            profile(
                DeviceProfile::new(id, label)
                    .with_rules(
                        names
                            .iter()
                            .map(|name| MatchRule::NameEqualsIgnoreCase((*name).into())),
                    )
                    .with_policy(ConnectionPolicy {
                        pairing: PairingPolicy::FixedPin(&[0, 0, 0, 0]),
                        reconnect: ReconnectPolicy::ControllerInitiated,
                        controller_opens_hid: true,
                        wait_for_encryption_before_inbound_hid: true,
                        become_master: false,
                        automatic_flush_slots: None,
                        host_identity: HostIdentity::NINTENDO,
                    })
                    .with_device(Arc::new(SwitchDevice::new(label))),
            )
        })
        .collect::<Vec<_>>();

    profiles.push(profile(
        DeviceProfile::new("nintendo.wiimote", "Wii Remote")
            .with_rules([MatchRule::NamePrefix("Nintendo RVL-CNT".into())])
            .with_bonding_modes([
                BondingMode::new("sync", "Red SYNC button"),
                BondingMode::new("one-two", "1 + 2 buttons"),
            ])
            .with_policy(ConnectionPolicy {
                pairing: PairingPolicy::LocalAddressPin,
                reconnect: ReconnectPolicy::ControllerInitiated,
                controller_opens_hid: true,
                wait_for_encryption_before_inbound_hid: false,
                become_master: true,
                automatic_flush_slots: Some(0),
                host_identity: HostIdentity::SNOWGLASS,
            })
            .with_bonding_policy(
                "one-two",
                ConnectionPolicy {
                    pairing: PairingPolicy::RemoteAddressPin,
                    reconnect: ReconnectPolicy::ControllerInitiated,
                    controller_opens_hid: true,
                    wait_for_encryption_before_inbound_hid: false,
                    become_master: true,
                    automatic_flush_slots: Some(0),
                    host_identity: HostIdentity::SNOWGLASS,
                },
            )
            .with_device(Arc::new(WiimoteDevice)),
    ));

    profiles.push(profile(
        DeviceProfile::new("nintendo.switch2", "Nintendo Switch 2 controller")
            .with_rules([
                MatchRule::Manufacturer(NINTENDO_COMPANY_ID),
                MatchRule::Service(SWITCH2_SERVICE_UUID),
            ])
            .with_bonding_modes([BondingMode::new("pseudo-oob", "Nintendo pseudo-OOB")])
            .with_policy(ConnectionPolicy {
                pairing: PairingPolicy::PseudoOob,
                reconnect: ReconnectPolicy::WakeAdvertisement {
                    backoff: Duration::from_secs(2),
                },
                controller_opens_hid: false,
                wait_for_encryption_before_inbound_hid: false,
                become_master: true,
                automatic_flush_slots: None,
                host_identity: HostIdentity::SNOWGLASS,
            })
            .with_device(Arc::new(Switch2Device::default())),
    ));

    for (id, name, product_ids) in [
        ("sony.dualshock3", "DualShock 3", &[0x0268, 0x042f][..]),
        (
            "sony.dualshock4",
            "DualShock 4",
            &[0x05c4, 0x09cc, 0x0ba0][..],
        ),
        ("sony.dualsense", "DualSense", &[0x0ce6, 0x0df2][..]),
    ] {
        profiles.push(profile(DeviceProfile::new(id, name).with_rules([
            MatchRule::Pnp {
                vendor_id: SONY_VENDOR_ID,
                product_ids: product_ids.to_vec(),
            },
        ])));
    }

    profiles.extend([
        profile(
            DeviceProfile::new("sony.hid", "Sony HID controller")
                .with_rules([MatchRule::PnpVendor(SONY_VENDOR_ID)]),
        ),
        profile(
            DeviceProfile::new("generic.hid", "Generic Bluetooth HID controller").with_rules([
                MatchRule::Service(HID_SERVICE),
                MatchRule::Service(BLE_HID_SERVICE),
                MatchRule::ClassOfDevice {
                    mask: 0x1f00,
                    value: 0x0500,
                },
            ]),
        ),
    ]);

    profiles
}
