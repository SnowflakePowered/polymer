//! BLE GATT transport through the operating-system-owned radio.

mod gatt;

pub use gatt::{
    BleDevice, BleGatt, GattInitWrite, GattProfile, Switch2Bond, Switch2Kind,
    default_adapter_address, scan_switch2, stop_scan,
};

use uuid::Uuid;

pub const SWITCH2_SERVICE_UUID: Uuid = Uuid::from_u128(0xab7de9be_89fe_49ad_828f_118f09df7fd0);
pub const SWITCH2_INPUT_UUID: Uuid = Uuid::from_u128(0xab7de9be_89fe_49ad_828f_118f09df7fd2);
pub const SWITCH2_JOYCON_LEFT_INPUT_UUID: Uuid =
    Uuid::from_u128(0xcc1bbbb5_7354_4d32_a716_a81cb241a32a);
pub const SWITCH2_JOYCON_RIGHT_INPUT_UUID: Uuid =
    Uuid::from_u128(0xd5a9e01e_2ffc_4cca_b20c_8b67142bf442);
pub const SWITCH2_PRO_INPUT_UUID: Uuid = Uuid::from_u128(0x7492866c_ec3e_4619_8258_32755ffcc0f8);
pub const SWITCH2_GAMECUBE_INPUT_UUID: Uuid =
    Uuid::from_u128(0x8261cba1_9435_420c_84d6_f0c75a2c8e4d);
pub const SWITCH2_COMMAND_UUID: Uuid = Uuid::from_u128(0x649d4ac9_8eb7_4e6c_af44_1ea54fe5f005);
pub const SWITCH2_RESPONSE_UUID: Uuid = Uuid::from_u128(0xc765a961_d9d8_4d36_a20a_5315b111836a);
pub const SWITCH2_RUMBLE_UUID: Uuid = Uuid::from_u128(0x289326cb_a471_485d_a8f4_240c14f18241);

/// Kept as a compatibility alias for callers that used the original name.
pub const SWITCH2_OUTPUT_UUID: Uuid = SWITCH2_COMMAND_UUID;

pub const SWITCH2_SPI_INIT: [u8; 16] = [
    0x21, 0x01, 0x00, 0x10, 0x00, 0x18, 0x00, 0x00, 0xd0, 0x3d, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00,
];

pub const fn switch2_feature_command(subcommand: u8, mask: u8) -> [u8; 12] {
    [
        0x0c, 0x91, 0x01, subcommand, 0x00, 0x04, 0x00, 0x00, mask, 0x00, 0x00, 0x00,
    ]
}

/// Sets exactly one of the four Switch 2 player LEDs. Sending this after a
/// direct GATT connection also replaces the controller's pairing animation
/// with a stable slot indicator.
pub const fn switch2_player_led_command(slot: usize) -> [u8; 16] {
    let bounded_slot = if slot > 3 { 3 } else { slot };
    let mask = 1u8 << (bounded_slot as u32);
    [
        0x09, 0x91, 0x01, 0x07, 0x00, 0x08, 0x00, 0x00, mask, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ]
}

/// Explicitly disables the firmware's player-LED flashing state. Some
/// controllers retain it after leaving their discovery animation.
pub const fn switch2_disable_led_flash_command() -> [u8; 12] {
    [
        0x09, 0x91, 0x01, 0x08, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]
}

/// Requests that a Switch 2 controller power down while retaining its bond.
/// The controller does not send a response; a normal button press later makes
/// it advertise the bonded host address for wake reconnection.
pub const fn switch2_sleep_command() -> [u8; 20] {
    [
        0x06, 0x91, 0x01, 0x02, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00,
    ]
}
