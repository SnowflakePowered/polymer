# First-party NSO SNES controller reconnect - solved

Status 2026-08-27 JST. Bonded button-wake reconnect now works on the dedicated
TP-Link/Realtek adapter. This document records the root cause, the evidence, and
the experiments that were ruled out, so the reasoning is not lost.

The same slave-role path has also been verified on a first-party Nintendo Switch
Online N64 controller; see "NSO N64 cross-controller validation" below.

## Root cause

On an inbound bonded reconnect the host accepted the ACL with
`Accept Connection Request` role `0x00`, taking the link master role away from
the paging controller.

A Nintendo Switch 1 controller refreshes the bonded link's encryption key over
LMP immediately after its HID control channel is configured, and only then
originates the HID interrupt PSM `0x13`. That refresh is a master-side link
manager operation. The RTL8761BU firmware does not implement it: HCI
`Refresh Encryption Key` (`0x0453`) is answered with Command Complete status
`0x01`, Unknown HCI Command. As master, the adapter therefore never completed the
transaction the controller was waiting for, so PSM `0x13` never appeared.

Leaving the controller as master lets it drive its own key refresh. Everything
after that follows immediately.

## Fix

`bins/polymer-bttest/src/worker.rs` accepts an inbound ACL from a Switch
controller with `HciHost::accept_connection_with_role(addr, become_master =
false)`. `BrEdrHid::attach_inbound_switch_with_frames` then waits for encryption
and accepts both controller-originated HID PSMs concurrently. The host issues no
capability probes and no security commands on this path.

Wii Remote and DualShock 3 inbound reconnects are unchanged and still take the
master role.

Observed working sequence, from the raw H4 capture:

```text
0.146  I  Connection Request from D4:F0:57:E5:1C:A9
0.146  O  Accept Connection Request, role 0x01 (remain slave)
0.159  I  Connection Complete, handle 0x0001        (no Role Change)
0.2xx  I  Link Key Request -> reply
0.255  I  Encryption Change, enabled
0.284  I  L2CAP ConnReq PSM 0x11
0.329  I  Encryption Key Refresh Complete, status 0x23 (LMP collision)
0.347  I  Encryption Key Refresh Complete, status 0x00
0.353  I  L2CAP ConnReq PSM 0x13
       -> both channels configured, switch::initialize succeeds,
          continuous native 0x30 reports
```

Page to both HID channels takes about 200 ms. The first key refresh attempt often
reports status `0x23` (LMP Error Transaction Collision) and the controller
retries successfully; this is normal and must not be treated as fatal. A few
encryption pause/resume cycles follow in the first seconds, then stop. Report
flow is continuous, with a worst-case inter-report gap of 56 ms.

## Raw H4 tracing

`crates/input/polymer-bluetooth/src/bredr/hci/trace.rs` records every command,
event, and ACL packet at the `UsbHci` boundary when `POLYMER_BT_TRACE` names an
output file:

```powershell
$env:POLYMER_BT_TRACE = "$env:TEMP\polymer-h4.txt"
cargo run -p polymer-bttest
```

The layout matches the BlueRetro reference logs, so captures can be diffed
against them directly. Link-key bytes in Link Key Request Reply and Link Key
Notification are zeroed, so a capture can be shared safely.

## Reference traces that settled this

All three known-good hosts show the same controller-initiated refresh between
control-channel configuration and the interrupt request. Two of them use the same
first-party `SNES Controller` at `5C:0C:E6:06:F1:D2`.

| Host | Controller | Gap after control config | Second Link Key Request |
| --- | --- | --- | --- |
| BlueRetro / ESP32, `blueretro_logs/blueretro_sw_2x_reconn.txt` | SNES Controller | 60 ms | no |
| Steam Deck / BlueZ, `sw_reconn.log` | SNES Controller | 86 ms | no |
| Ubuntu / BlueZ, `ubuntu1804_sw_btn_page.log` | Pro Controller | 5 ms | yes |

The absence of a second Link Key Request in the first two proves the controller
performs a pure encryption key refresh rather than a re-authentication.

`sw_reconn.log` and `ubuntu1804_sw_btn_page.log` are btsnoop binaries despite
their extensions. `sw_reconn.log` uses the BlueZ monitor datalink, where the
record flags field carries the monitor opcode rather than a direction bit.

## Experiment matrix

Every variant below was run on the first-party controller at
`D4:F0:57:E5:1C:A9`, each with a raw H4 capture.

### Ruled out, all with the host as master

1. **Accept both controller-originated PSMs.** Control configured, interrupt
   never arrived.
2. **Open interrupt from the host immediately.** No response, timeout.
3. **Race/adopt both directions.** Avoided duplicate control channels but did not
   produce PSM `0x13`.
4. **Match BlueRetro's page-time capability probes.** Remote name, supported
   features, extended features page 1, and the L2CAP extended-feature request all
   succeed and change nothing.
5. **Wait only for the controller-originated interrupt.** Nothing arrives; the
   link eventually dies of supervision timeout, reason `0x08`.
6. **Explicit HCI Refresh Encryption Key (`0x0453`).** Command Complete with
   status `0x01`, Unknown HCI Command. An earlier note recorded this as a success;
   it is a hard rejection. The old `HciHost::execute` returned `Ok` for any
   Command Complete without inspecting the status byte, which is how it was
   misread. This gap is now fixed: Command Complete and Command Status failures
   both return `Error::HciStatus`, with regression tests for nonzero and missing
   status bytes.
7. **Authentication Requested after PSM `0x11`.** Produces a Link Key Request,
   Authentication Complete `0x00`, and Encryption Key Refresh Complete `0x00`.
   The controller then closes PSM `0x11` about 590 ms later. It rejects being
   challenged once its control channel exists.
8. **Host-open PSM `0x13` after a successful key refresh.** The controller answers
   Connection Response Pending and never sends the follow-up Success.
9. **Re-pair immediately before reconnect.** No change.
10. **Reject the inbound ACL and page back from the host.** HCI `0x04` Page
    Timeout. Rejecting the page does not leave the controller connectable.
11. **No host probing at all.** With the host sending nothing but the ACL accept
    and the L2CAP responses, the capture is byte-identical to BlueRetro's through
    control-channel configuration and the controller still stalls. This proved the
    divergence was below HCI.
12. **Re-key before the control channel exists.** Authentication Requested
    immediately after Encryption Change yields a successful key refresh, and the
    controller does *not* tear down, but it still never opens PSM `0x13`. A
    refreshed key alone is not sufficient; the controller must perform the refresh
    itself.

### The variant that worked

13. **Remain slave.** `Accept Connection Request` with role `0x01`, no role
    switch, no probes. The controller keeps the master role, performs its own key
    refresh, and opens PSM `0x13` 6 ms later.

## NSO N64 cross-controller validation

A first-party `N64 Controller` at `CC:5B:31:3E:86:F7` was freshly paired, stored
with identity token `nso-n64`, disconnected, and reconnected by button wake on
the same TP-Link/Realtek adapter. The default Switch inbound path succeeded
without controller-specific changes:

```text
61.034  I  Connection Request
61.034  O  Accept Connection Request, role 0x01 (remain slave)
61.051  I  Connection Complete, handle 0x0001
61.152  I  L2CAP ConnReq PSM 0x11
61.203  I  Encryption Key Refresh Complete, status 0x23
61.221  I  Encryption Key Refresh Complete, status 0x00
61.227  I  L2CAP ConnReq PSM 0x13
61.236  O  Switch report-mode initialization begins
61.267  I  first native 0x30 report
```

`switch::initialize` completed, player LED 1 became steady, and the redacted H4
capture contained 4,912 continuous native `0x30` reports with a maximum observed
inter-report gap of 51 ms. This confirms the first `0x23` refresh collision is
normal for both tested NSO models and must be ignored while the controller
retries.

## Notes worth keeping

- `EncryptionKeyRefreshComplete` event `0x30` is parsed and logged.
- `HciHost::authenticate()` accepts either a successful Encryption Change or a
  successful Encryption Key Refresh Complete after Set Connection Encryption.
- L2CAP Configuration Response uses the peer/source CID, not the local
  destination CID.
- Incoming Configuration Requests with MTU 800 are accepted.
- Host Configuration Requests use no options, matching BlueRetro and BlueZ.
- Preferred host CIDs are `0x0080` for control and `0x0090` for interrupt.
- Early HCI and L2CAP receivers are subscribed before ACL acceptance.
- `COMMAND_TIMEOUT` is 6 s. A cold RTL8761BU takes just over 3 s to complete its
  first Reset, which used to make every other adapter open fail with
  "timed out while waiting for HCI command completion".
- WinUSB/libusbK gives raw access to the USB transport only. HCI is the boundary;
  LMP and the baseband run on the dongle. A missing controller-side facility
  cannot be worked around from the host. The RTL8761BU does run a host-loaded
  firmware patch, so a different patch blob is the only lever if a controller-side
  gap ever has to be closed.

## Hardware and persistent identity

- Windows host.
- OS-owned BLE adapter: WinRT (motherboard MediaTek radio).
- Dedicated BR/EDR adapter: TP-Link UB500-class device bound to libusbK.
- Dedicated adapter address: `50:3D:D1:C6:4D:71`.
- HCI/LMP version: 13; HCI revision `0xDDD5`; LMP subversion `0xC761`;
  manufacturer 93 (Realtek).
- ACL pool: 1021-byte packets, 6 buffers.
- Controller address: `D4:F0:57:E5:1C:A9`.
- Remote name: `SNES Controller`.
- Stored controller identity token: `nso-snes`.
- Current stored link-key type is `0x04` (unauthenticated combination key,
  P-192), as expected for SSP Just Works.

Persistent files:

- `%LOCALAPPDATA%\Polymer\bluetooth-link-keys.txt`
- `%LOCALAPPDATA%\Polymer\bluetooth-controller-identities.txt`

Do not print the link-key value in logs or issue reports.

## Repository locations

- Bluetooth crate: `crates/input/polymer-bluetooth`
- Test viewer: `bins/polymer-bttest`
- Viewer worker and reconnect policy: `bins/polymer-bttest/src/worker.rs`
- BR/EDR HID setup: `crates/input/polymer-bluetooth/src/bredr/hid.rs`
- L2CAP actor: `crates/input/polymer-bluetooth/src/bredr/l2cap/link.rs`
- HCI host/security: `crates/input/polymer-bluetooth/src/bredr/hci/host.rs`
- HCI event parser: `crates/input/polymer-bluetooth/src/bredr/hci/event.rs`
- Nintendo initialization and sleep command:
  `crates/input/polymer-bluetooth/src/bredr/switch.rs`

Both Bluetooth directories are currently untracked in Git. The surrounding
worktree is heavily dirty with unrelated user changes. Do not reset or reformat
the whole repository.

## Switch OS `btdrv` findings (2026-08-27)

> The TBFC findings below describe how a Switch console *pages a sleeping
> controller*. They are accurate but are not needed for the controller-pages-host
> direction this project uses: BlueRetro reconnects the same first-party SNES
> controller with a plain Accept Connection Request and no vendor commands.


The actual Nintendo `btdrv` and `btm` content archives were downloaded from the
Nintendo update CDN and inspected locally. No firmware, decrypted executable, or
key material has been added to this repository.

### Artifacts and verification

The usable reference is firmware 19.0.0. Its Bluetooth program NCA uses master
key generation `0x12`, which is covered by the supplied key set. Firmware 22.0.0
was also downloaded and hash-verified, but its program NCA uses generation
`0x15`, so its executable was not decrypted.

Firmware 19.0.0 Bluetooth title (`010000000000000B`):

- content-meta NCA ID: `a042179f2dc68775db827f368afd8d5c`
- content-meta SHA-256:
  `a042179f2dc68775db827f368afd8d5c4c619c34c42c2934e963e57c74e0f388`
- program NCA ID: `0570a5bd69f7c787275f994652e83d56`
- program SHA-256:
  `0570a5bd69f7c787275f994652e83d56f59af95e0850be0ee8dc7322390f8d69`

Firmware 19.0.0 Bluetooth manager title (`010000000000002A`):

- content-meta NCA ID: `292834a35395a1021a086630dedb877e`
- content-meta SHA-256:
  `292834a35395a1021a086630dedb877e4bfd80fcee517415b7ecdff2e3a9eb6f`
- program NCA ID: `d86ce94df60a9188b38319899c0edd9a`
- program SHA-256:
  `d86ce94df60a9188b38319899c0edd9aa9de10f28c56d8fec565b60f1099c227`

The local research tree is:

```text
C:\Users\ronny\AppData\Local\Temp\polymer-switch-btdrv-reference
```

It contains the original NCAs, extracted ExeFS files, uncompressed NSOs, and
section binaries used for disassembly. Treat this as temporary reference data,
not as project source.

### What `TriggerConnection` really does

Switchbrew documents command 23, `TriggerConnection`, as two operations:

1. configure TBFC with a timeout;
2. open the paired HID host connection with security mask `0x12` and Broadcom
   mask `BSA_HH_BRCM_TBFC_PAGE`.

The firmware 19.0.0 `btdrv` binary confirms and fills in the first operation.
The service wrapper at text offset `0x2F580` constructs BSA extension message
`0x8CE`, places the caller's `u16` timeout at message offset `0x176`, and sets
flag bit 12. Its worker dispatches that flag to the routine at `0x4DB20`.

That routine sends Broadcom vendor HCI opcode `0xFCC2` with a 14-byte payload:

```text
01 00 18 4E 20 00 02 TT TT 00 02 03 00 00
                     ^^^^^
                     timeout, little-endian
```

Decoded fields, matching the Switchbrew reverse engineering:

```text
Enable              = 1
Frequency1          = 0x00
Frequency2          = 0x18
Frequency3          = 0x4E
AccessCodeLength    = 0x20
HostScanInterval    = 0x0200
HostTriggerTimeout  = timeout
HidScanInterval     = 0x0200
HidScanRetry        = 3
DontDisturb         = 0
WakeUpMask          = 0
```

`btm` is the policy layer using this facility. The public service interface shows
that it enables TBFC scan, asks `GetPendingConnections` for controller wake
triggers, and invokes `TriggerConnection` for a paired device. An inbound wake
therefore is not treated as a complete ordinary HID ACL connection. It is the
trigger for a TBFC host-page operation.

### Consequence for this adapter

The dedicated TP-Link reports manufacturer 93 and Realtek firmware. Opcode
`0xFCC2` is a Broadcom vendor command; its behavior cannot be assumed on Realtek
and it must not be sent as if it were a standard HCI command. This means the
Switch OS reconnect sequence cannot be copied literally on the current dongle.

This explains the two observed failures:

- accepting the wake as a normal inbound HID connection gets only PSM `0x11`;
- rejecting it and issuing an ordinary HCI Create Connection gets Page Timeout
  `0x04`, because the controller is waiting for the TBFC response rather than
  staying in normal page scan.

This does not yet prove NSO reconnect is impossible with Realtek. BlueRetro uses
an ESP32 rather than the Switch's Broadcom controller, so a controller-compatible
generic sequence may exist. It does prove that more changes to ordinary L2CAP
configuration are unlikely to reproduce Nintendo's path.

### Outcome

These experiments were superseded. The controller-pages-host direction needed no
TBFC equivalent: it only needed the host to stop taking the master role. The
`0xFCC2` payload above remains the reference if host-initiated wake of a sleeping
controller is ever implemented.

## Most valuable next steps

1. Confirm the same inbound slave-role path on the other NSO controllers (NES,
   N64, Genesis) and on Joy-Cons and the Pro Controller. `switch.rs` already
   distinguishes them, and the reference traces show the same reconnect shape.
2. Decide whether the non-Switch inbound path (Wii Remote, DualShock 3) benefits
   from the same role choice. It currently takes the master role and works, so do
   not change it without a capture.
3. Consider whether the outbound Switch connection should also leave the
   controller as master. Pairing works as master today, so this is not urgent.

## References

Primary/reference implementations used so far:

- BlueRetro HCI:
  <https://github.com/darthcloud/BlueRetro/blob/master/main/bluetooth/hci.c>
- BlueRetro L2CAP:
  <https://github.com/darthcloud/BlueRetro/blob/master/main/bluetooth/l2cap.c>
- BlueRetro trace repository:
  <https://github.com/darthcloud/bt_traces>
- BlueZ HID input profile:
  <https://github.com/bluez/bluez/blob/master/profiles/input/device.c>
- BlueZ reconnect-mode documentation:
  <https://github.com/bluez/bluez/blob/master/doc/org.bluez.Input.rst>
- Nintendo Switch reverse engineering, subcommand `0x06`:
  <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/bluetooth_hid_subcommands_notes.md>
- Independent Switch HID connection-direction notes:
  <https://github.com/cajunpanda/bluetooth-nes-advantage/blob/main/docs/switch_pro_protocol.md>
- Switchbrew Bluetooth driver services, including `TriggerConnection`,
  `EnableTbfcScan`, and `GetPendingConnections`:
  <https://switchbrew.org/wiki/Bluetooth_Driver_services>
- Nintendo update CDN protocol reference:
  <https://github.com/kinnay/NintendoClients/wiki/Atumn-Server>
- `hactool`, used to verify/decrypt/extract the NCAs:
  <https://github.com/SciresM/hactool>

Local reference traces already available on this machine:

```text
C:\Users\ronny\AppData\Local\Temp\polymer-bt-traces-reference\blueretro_logs\blueretro_sw_joycon_l_page.txt
C:\Users\ronny\AppData\Local\Temp\polymer-bt-traces-reference\blueretro_logs\blueretro_sw_joycon_r_page.txt
C:\Users\ronny\AppData\Local\Temp\polymer-bt-traces-reference\blueretro_logs\blueretro_sw_2x_reconn.txt
C:\Users\ronny\AppData\Local\Temp\polymer-bt-traces-reference\ubuntu1804_sw_btn_page.log
```

The `ubuntu1804_sw_btn_page.log` file is actually binary btsnoop data despite its
extension. It is not directly searchable as text. `tshark` and `btmon` were not
installed when this handoff was written.

## Build and hardware-test procedure

Targeted validation commands:

```powershell
cargo fmt -p polymer-bluetooth -p polymer-bttest
cargo test -p polymer-bluetooth -p polymer-bttest
cargo clippy -p polymer-bluetooth -p polymer-bttest --all-targets -- -D warnings
cargo run -p polymer-bttest
```

Current automated result: 38 Bluetooth library tests plus 5 viewer tests pass;
targeted clippy is clean.

Do not run `cargo fmt --all`: an unrelated GUI file has had syntax problems during
this work, and the repository contains unrelated user changes.

Before rebuilding, close only the exact viewer executable:

```powershell
$viewerPath = (Resolve-Path 'target\debug\polymer-bttest.exe').Path
Get-CimInstance Win32_Process |
  Where-Object { $_.ExecutablePath -eq $viewerPath }
```

Use `CloseMainWindow()` first and force-stop only that verified exact path if it
does not exit.

The adapter used to time out during the first HCI initialization after every
rebuild. That was `COMMAND_TIMEOUT` at 3 s against a cold RTL8761BU Reset that
takes just over 3 s; the timeout is now 6 s and the first open succeeds. Never run
two viewer instances concurrently.
