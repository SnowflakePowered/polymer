# snowglass-bluetooth

Userspace Bluetooth controller lifecycle and raw HID transport for Snowglass.
Controller input interpretation, mapping, pointer transforms, and UI state do
not belong in this crate.

## Radio ownership

Snowglass deliberately uses two radios:

- BLE remains on an operating-system-owned adapter through btleplug/WinRT.
- BR/EDR uses the first USB Bluetooth controller bound to WinUSB. An explicit
  VID/PID or serial selector can be supplied when more than one is present.

Changing a Windows adapter to WinUSB removes it from the Windows Bluetooth
stack. Do not rebind the only radio used by the operating system for keyboards,
mice, audio, or BLE.

## Architecture

```text
src/
├── transport/
│   ├── ble/       OS GATT transport
│   └── bredr/     USB HCI, ACL, L2CAP, SDP, pairing, raw HID
├── devices/
│   ├── nintendo/  concrete Nintendo controllers and proprietary protocols
│   └── sony/      concrete PlayStation controllers
├── firmware/      firmware registry and embedded Realtek loader
├── manager.rs     discovery, bonding, lifecycle events, raw reports
└── store.rs       versioned controller/bond registry
```

Each concrete controller implements the object-safe `ControllerDevice` facade.
The manager identifies one implementation through `ControllerRegistry`, then
uses that same object for transport configuration, bonding, reconnect
validation, synchronization, player LEDs, sleep, and raw HID output. Shared
family protocols are private implementation details rather than runtime model
enums.

`BluetoothManager` exposes async scan/connect/send/sleep/disconnect/forget
operations and a broadcast stream of lifecycle events and unmodified HID
reports. `FileControllerStore` persists its versioned registry atomically.

Minimal SDP queries identify HID and PnP records before a caller applies
product-specific policy. Unknown HID devices use the generic profile, while
unattended reconnect is restricted to persisted controllers.

## Realtek firmware

The binary embeds the linux-firmware payloads and configuration aliases needed
for RTL8723A/B/D, RTL8761A/BU, RTL8821A/C, RTL8822B/CU, RTL8851BU,
RTL8852AU/BU/CU/BTU, and RTL8922AU. Both `Realtech` epatch v1 and `RTBTCore`
epatch v2 containers are parsed. No installed chipset driver or runtime
firmware download is required.

Source revision, hashes, and the Realtek redistribution license are recorded
under `src/firmware/assets`.

## Probe

With no arguments, the probe opens the first WinUSB Bluetooth adapter. Optional
hexadecimal VID/PID arguments select a particular adapter:

```text
cargo run -p snowglass-bluetooth --example hci_probe
cargo run -p snowglass-bluetooth --example hci_probe -- 2357 0604
```

The probe resets and initializes only the dedicated adapter, reads its limits,
loads matching embedded firmware when needed, and exits without pairing a
controller.
