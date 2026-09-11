# polymer-dshidmini

Windows-only native leaf for DsHidMini USB pairing. Public methods expose owned
device identities, address snapshots, and a verified host-address write. The
Bluetooth crate stays free of unsafe code and performs blocking calls on its
worker pool.

The protocol follows upstream [IPC.h](https://github.com/nefarius/DsHidMini/blob/v3.3.1187.0/sys/IPC.h),
[IPC.Device.c](https://github.com/nefarius/DsHidMini/blob/v3.3.1187.0/sys/IPC.Device.c),
and [device properties](https://github.com/nefarius/DsHidMini/blob/v3.3.1187.0/include/DsHidMini/dshmguid.h).
Commands use the named mutex and request/reply events, with bounded waits and
reply validation. After a reply timeout this process refuses subsequent writes
because the protocol has no transaction IDs to distinguish late replies.

Device selection is revalidated before sending. Drivers exposing `IpcSlotIndex`
provide the target directly; the legacy layout is supported only for version
3.3.1187.0 with exactly one connected device and one occupied HID slot. Multiple
legacy devices are rejected rather than assigning slots from enumeration order.
Hot-unplug during a legacy command is not atomic with the driver's slot allocation;
keep the selected controller connected through completion.

No input stream is opened. HID emulation settings, driver bindings and driver
configuration files are left to their existing owners. The host address read
operation reads the driver's last successful hardware readback; pairing requests
a fresh hardware write/read before returning success.
