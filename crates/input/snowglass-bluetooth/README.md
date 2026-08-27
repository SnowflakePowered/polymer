# snowglass-bluetooth

Userspace Bluetooth controller transports for Snowglass. This crate deliberately
does not expose SDL devices or GUI state; those are consumers of the normalized
report streams defined here.

## Radio ownership

Use two radios:

- Leave the motherboard radio on the operating-system Bluetooth stack for BLE.
- Dedicate a USB dongle to BR/EDR. On Windows it must use the **WinUSB** driver.
  `libusbK.sys` is not compatible with the pure-Rust `nusb` Windows backend.
  Replacing the driver removes that dongle from Windows Bluetooth, so never do
  this to the only radio used by keyboards, mice, audio, or other peripherals.
- On Linux the USB H2 backend is currently available; an `HCI_CHANNEL_USER`
  backend should be preferred when added because it does not require rebinding.

Dolphin Bluetooth Passthrough must be disabled: it competes for exclusive
ownership of the dedicated USB interface. Real Wii Remote mode and Continuous
Scanning must also be disabled when the Snowglass bridge is active.

## Implemented foundation

- USB H2 endpoint discovery and exclusive interface claims, including SCO alt 0
- HCI command/event transport, initialization, inquiry, names, ACL connect/accept
- controller-reported ACL packet sizing and completed-packet credit accounting
- ACL fragmentation and per-handle L2CAP reassembly
- L2CAP signaling, dynamic channel negotiation, configuration, and disconnect
- HID control/interrupt PSM transport with a raw input/output report stream
- SSP/PIN/link-key event handling and a persistent link-key store
- explicit no-auth inbound ACL acceptance for DualShock 3 reconnection
- OS-owned BLE scanning and proprietary GATT subscribe/init/write transport
  with per-backend exponential reconnect cooldown
- Switch 2 advertisement identification for Joy-Con 2, Pro Controller 2, and
  NSO GameCube (`0x2073`), including model-specific notification UUIDs
- immediate Switch 2 calibration initialization, safe feature enable sequence,
  and a throughput-optimized OS connection-interval request
- Nintendo pseudo-OOB Switch 2 bonding with a fresh LTK, verified AES challenge,
  controller-side commit, retained-bond controller sleep, and normal-button
  wake reconnection

Run the hardware probe (VID/PID are hexadecimal and default to TP-Link
`2357:0604`):

```text
cargo run -p snowglass-bluetooth --example hci_probe -- 2357 0604
```

The probe is intentionally non-destructive. It resets the dedicated HCI
controller, reads its features, address, and packet-buffer limits, enables page
scan, and exits. It does not inquire, pair, or connect to a controller.

## Public layers

- `Transport` is the device-parser boundary. It emits controller-native HID
  reports and carries output reports in the reverse direction.
- `HciHost` owns one dedicated BR/EDR adapter and can be shared by multiple
  `BrEdrHid` instances.
- `Link` owns signaling for one ACL handle. Its actor serializes connection and
  configuration changes while independently routing high-rate channel data.
- `PairingAgent` responds to authentication events. Configure Wii Remote PIN
  mode per address before inquiry/pairing.

Controller report parsing, calibration, fusion, stable virtual slots, and SDL
virtual joystick publication belong above this crate and are intentionally not
wired here yet.
