# polymer-bttest

Small egui utility for exercising `polymer-bluetooth` without the Polymer
input or emulator integration layers.

```text
cargo run -p polymer-bttest
```

The Classic tab opens the first WinUSB Bluetooth adapter. It can
run BR/EDR inquiry, resolve controller names, select Wii/SSP pairing policy,
open HID control and interrupt channels, display raw reports, and send raw
output reports. Link keys persist under `%LOCALAPPDATA%\Polymer`.

### USB cable pairing

In **Classic BR/EDR → USB cable pairing**:

1. Connect a DualShock 3/SIXAXIS (`054c:0268`) with a USB data cable. On Windows,
   **DsHidMini with IPC enabled** supports pairing in SXS and XInput modes.
   The panel enumerates the physical device as **DsHidMini IPC**, independently
   of its emulated HID identity. WinUSB controllers remain supported as well.
   Windows' standard `HidUsb` driver blocks the raw `F2`/`F5` pairing reports.
   The dedicated Bluetooth adapter still needs its own WinUSB binding.
2. Click **Refresh USB controllers**, then **Read addresses** on the desired
   device. The panel shows its Bluetooth address, saved host, and the dedicated
   adapter address that will replace that host.
3. Click **Pair to dedicated adapter**. The tool verifies address readback and
   persists a keyless DS3 reconnect record in the shared `input.db`. Pairing is
   disabled if the persistent registry or dedicated adapter is unavailable.
4. Unplug USB and press PS to initiate the wireless connection. The saved host
   survives closing the tool and unplugging the cable; reconnecting USB does
   not require pairing again unless the controller's host has changed. DsHidMini's
   automatic pairing settings can overwrite the host when USB is reconnected;
   disable automatic pairing or configure the dedicated adapter as its custom host.

After cable provisioning, DS3 button-wake connections accept both HID channels,
enable Bluetooth input with feature `F4`, set the player LED, and wait for the
first 49-byte input report. Raw packets appear in the Classic tab. The Bluetooth
input HAL decodes buttons, sticks, all twelve pressures, acceleration, yaw rate,
and battery status. USB gameplay remains handled by SDL, but
changing the controller's driver can affect wired input. Restore a compatible
USB input driver after pairing if needed; this does not erase its saved host.
If registration fails after the USB write, the panel reports that the hardware
was paired but registration failed; read the addresses and retry. An already
matching host is verified without another write on the raw HID/WinUSB paths;
DsHidMini IPC requests a fresh write and readback. Switch Pro cable pairing can
use the same session interface but is not implemented yet.

`cargo run -p polymer-bluetooth --example cable_probe` performs read-only
enumeration and address inspection, including backend and operation diagnostics.

`cargo run -p polymer-bttest --example cable_pair -- <controller Bluetooth address>`
uses the same pairing coordinator and persistent database as the panel, and selects
the dedicated adapter for DS3 or the default OS BLE adapter for Switch 2. Close
other applications using the selected adapter first.

For Switch 2 controllers, the **USB cable pairing** panel is available in both
radio tabs. Connect a Joy-Con 2, Switch 2 Pro, or NSO GameCube controller over USB,
refresh, read its addresses, then select **Pair to OS BLE adapter**. This requires
no dedicated BR/EDR radio. It uses command interface 1 and leaves HID interface 0
alone. After verified pairing, unplug USB and press HOME (or another normal wake
button); the BLE wake listener is armed immediately and the bond survives restart.

`cargo run -p polymer-bttest --example cable_pair -- --list` inspects the connected
USB controllers without changing their saved hosts. Supply one of the printed
controller addresses instead of `--list` to pair it and save the reconnect record.

For DsHidMini 3.3.1187.0, connect only one DsHidMini controller during pairing:
that version does not expose a stable device-to-IPC-slot property. The backend
checks the sole occupied shared-memory slot instead of assuming enumeration order.
Newer drivers exposing `IpcSlotIndex` can identify individual slots directly.

`cargo run -p polymer-bttest --example ds3_bluetooth -- <controller Bluetooth address>`
performs a bounded inbound-connection and activation check for a persisted DS3,
then collects 100 raw input reports and disconnects. Close the GUI test tool to
release the adapter before running it, unplug USB, and press PS when it prints
`READY`.

The Switch 2 BLE tab uses an OS-owned adapter (the motherboard MediaTek radio
on the development machine). **Change controller order…** opens a combined
four-player setup screen for both radios. Entering the screen sleeps connected
Nintendo controllers where supported, disconnects the remaining sessions, and
then listens for controllers in the order they wake. Hold SYNC for a fresh
controller; recognized Nintendo BR/EDR names and Switch 2 GATT advertisements
are paired automatically without selecting a device or pressing a software
pair button.
It recognizes Joy-Con 2 L/R, Pro Controller 2, and the
NSO GameCube controller, connects to their proprietary GATT service, performs
the mandatory subscribe/init sequence, and displays raw notifications. The
GameCube device uses its model-specific report `0x0A` characteristic so its
analog trigger fields are present.

Arbitrary BLE command writes remain disabled. When a controller is connected
from its all-zero-host pairing advertisement, the viewer does perform the
specific Nintendo pseudo-OOB bond sequence: it reads the OS adapter address,
generates fresh key and challenge material, verifies the controller's AES
response, and only then commits the bond. Standard Windows SMP pairing must not
be used. After the initial bond, a normal button wake advertisement reconnects
the controller to its previous player slot and restores its steady player LED.
Use **Sleep selected controller (keep wake armed)** to test that path: it sends
Nintendo's retained-bond shutdown command, closes the local GATT session, and
immediately waits for a fresh wake advertisement. After the LEDs turn off,
press HOME or another normal controller button. Do not use SYNC for wake.
Bond metadata, controller profile IDs, adapter indices, and player slots persist
in Polymer's shared `input.db`.
The viewer restores them on its next launch and immediately waits for their
wake advertisements; discovery does not need to be enabled again.
When upgrading from a build that did not yet create this registry, startup
also listens for wake advertisements addressed to the current OS Bluetooth
adapter. Pressing HOME recovers and persists that existing bond automatically.
All BLE discovery—including multiple dormant controller wake listeners—uses a
single adapter-wide scan gate. Wake windows rotate fairly between controllers,
so connecting one controller cannot cancel another controller's WinRT scan.
Player LEDs use the acknowledged Bluetooth bitmask command. Joy-Con/Pro pads
also receive flash-disable; the NSO GameCube controller does not, because its
firmware acknowledges that command while incorrectly making the pattern dark.

For Wii Remote red-SYNC pairing, choose the red-SYNC PIN method before
connecting; it uses the dedicated adapter's reversed binary address. The 1+2
method is temporary and uses the Wii Remote's reversed binary address. For a
cable-paired DualShock 3 or a bonded Wii Remote reconnect, enable inbound
reconnect acceptance and press its power button.

The controller-order screen runs BR/EDR inquiry on the dedicated adapter while
the OS adapter scans for Switch 2 BLE advertisements. Each BR/EDR cycle
alternates the Wii's Limited Dedicated Inquiry Access Code (LIAC), matching
Dolphin, with the normal GIAC used by other controllers. Inquiry is not filtered
inside the controller because doing so hides Wii Remote clones that advertise
an incorrect Class of Device. Leaving the screen cancels both discovery loops
and restores dormant bonded Switch 2 wake listeners.

Dolphin Bluetooth Passthrough, Real Wii Remote mode, and Continuous Scanning
must remain disabled while this application owns the dedicated adapter.

Wii and Switch report decoding lives in this binary. `WiimoteTelemetryDecoder`
owns the userspace Wii decoder, and the airmouse fusion code is nested beneath
that decoder. The Bluetooth crate supplies only connection-time setup and raw
HID reports.
