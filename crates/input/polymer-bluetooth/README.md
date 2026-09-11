# polymer-bluetooth

Userspace Bluetooth controller lifecycle and raw HID transport for Polymer.
Controller input interpretation, mapping, pointer transforms, and UI state do
not belong in this crate.

## Radio ownership

Polymer deliberately uses two radios:

- BLE remains on an operating-system-owned adapter through btleplug/WinRT.
- BR/EDR uses the first USB Bluetooth controller bound to WinUSB. An explicit
  VID/PID or serial selector can be supplied when more than one is present.

Changing a Windows adapter to WinUSB removes it from the Windows Bluetooth
stack. Do not rebind the only radio used by the operating system for keyboards,
mice, audio, or BLE.

## Architecture

```text
src/
├── cable_pairing/ USB pairing handshakes and verified wireless registration
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
reports. `SqliteControllerStore` persists bonds in Polymer's shared
`input.db`; `FileControllerStore` remains available for standalone tooling.

Minimal SDP queries identify HID and PnP records before a caller applies
product-specific policy. Unknown HID devices use the generic profile, while
unattended reconnect is restricted to persisted controllers.

## USB cable pairing

`CablePairing::enumerate()` finds supported USB interfaces. Open a candidate
to obtain a `CablePairingSession`, inspect it with `read_pairing()`, and pass
the session and a `CablePairingHost` to `CablePairing::pair()`. Use the address
from the dedicated BR/EDR `HciHost::info()`, rather than the OS BLE adapter.
The coordinator saves the verified controller profile and optional link key
to the supplied `ControllerStore`, enables reconnect, and retains its player
slot. A registration error after verification explicitly reports that the
controller hardware has already been paired; the operation can be repeated.

The first implementation supports Sony DS3/SIXAXIS (`054c:0268`): read the
controller address with feature `F2`, read/write the host address with `F5`,
and verify both addresses before committing the registry entry. The address
remains stored after closing the USB handle or unplugging the cable. DS3 uses
no standard Bluetooth link key. Other cable-pairing protocols can implement
the session trait and return optional bond material without implementing USB
input. Switch Pro cable pairing is not implemented yet.

DsHidMini-bound USB controllers use the safe `polymer-dshidmini` interface to
enumerate the physical driver device, read its address properties, and issue
`PAIR_TO` over the driver's shared-memory IPC. This works independently of SXS
or XInput mode. Both reply statuses and the refreshed host address must match
before the common coordinator persists a reconnect record. Its HID children
are excluded from raw feature-report enumeration. IPC must already be enabled;
the backend does not install drivers, request elevation, or change HID mode.
Version 3.3.1187.0 requires a single connected DsHidMini device because it lacks
the newer `IpcSlotIndex` property. DsHidMini automatic pairing can replace the
stored host on reconnect; configure its custom host or disable automatic pairing
if the dedicated adapter should remain the host.

Other USB access uses HIDAPI where supported and raw endpoint-zero control transfers
through `nusb` for WinUSB-bound DS3 controllers on Windows. Windows `HidUsb`
rejects the DS3's undeclared `F2`/`F5` pairing reports with error 87, regardless
of buffer size. Bind the controller to WinUSB for this pairing path; `polymer-bttest`
provides a per-controller switch button that attempts this without requesting
elevation and reports Windows access errors. Restore a compatible USB input driver afterward if
needed for SDL; the stored wireless host is unaffected. No USB input stream is opened, and closing a
pairing session sends no reset/unpair command.

The DS3 profile uses cable-provisioned, controller-initiated reconnection without
requiring an encrypted link. L2CAP recognizes the interrupt channel's legacy
QoS option. Once both channels are configured, the session sends HIDP
`53 F4 42 03 00 00` on the control channel, then a `52 01 ...` output report for
the player LED and rumble state. Each SET_REPORT requires a successful handshake;
initialization also waits for a 49-byte input report. Reports retain the raw DS3
report ID (`01`) after the transport removes the HIDP `A1` prefix. Decoding into
the input HAL's gamepad state is separate work.

The reference for DS3 report sizes and address ordering is
[BlueZ's sixaxis plugin](https://github.com/bluez/bluez/blob/master/plugins/sixaxis.c).
`polymer-bttest` exposes the workflow in its Classic BR/EDR tab.
Raw USB host writes use `wValue=0x03f5` and an `01 00 <address>` payload, as in
[sixpair](https://www.pabr.org/sixlinux/sixpair.c); the HIDAPI report prefix is
not copied into that raw payload. `cable_probe` is a read-only diagnostic example.

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
cargo run -p polymer-bluetooth --example hci_probe
cargo run -p polymer-bluetooth --example hci_probe -- 2357 0604
```

The probe resets and initializes only the dedicated adapter, reads its limits,
loads matching embedded firmware when needed, and exits without pairing a
controller.
