//! BLE GATT transport through the operating-system-owned radio.

mod gatt;

pub use gatt::{
    BleDevice, BleGatt, GattNotification, GattNotificationReceiver, GattProfile,
    default_adapter_address, scan, stop_scan,
};
