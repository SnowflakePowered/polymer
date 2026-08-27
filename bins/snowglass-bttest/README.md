# snowglass-bttest

Small egui utility for exercising `snowglass-bluetooth` without the Snowglass
input or emulator integration layers.

```text
cargo run -p snowglass-bttest
```

The Classic tab is pinned to the dedicated TP-Link `2357:0604` adapter. It can
run BR/EDR inquiry, resolve controller names, select Wii/SSP pairing policy,
open HID control and interrupt channels, display raw reports, and send raw
output reports. Link keys persist under `%LOCALAPPDATA%\Snowglass`.

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
Bonded controller addresses, controller kinds, adapter indices, and player
slots persist in `%LOCALAPPDATA%\Snowglass\bluetooth-ble-controllers.txt`.
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
