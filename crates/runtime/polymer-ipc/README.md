# Polymer IPC

Windows retains its AF_UNIX control channel and duplicated mapping handles.
macOS uses NSXPC for messages and native resource transfer. The existing
`LocalSocket` names remain for shared callers; they do not create sockets on
macOS. Input and frame metadata still use the shared `bcast` rings. Mapping
offers carry `NSFileHandle` attachments, not descriptor integers from another
process.

The macOS `xpc::Connection` API sends bounded packets containing bytes and
optional file, IOSurface array, producer shared-event handle, and consumer
shared-event handle attachments. Metal resource import and rendering remain
the frontend's responsibility. IOSurfaces are non-global, and event handles
travel as NSXPC objects rather than serialized archives. Raw HID uses the same
XPC transport on macOS and retains named pipes on Windows.

`org.polymer.ipc` is a per-user launchd service that introduces named anonymous
listeners. It only transfers `NSXPCListenerEndpoint` capabilities: after lookup,
connections run directly between peers. Registrations disappear when their
owner disconnects; existing direct connections do not depend on the broker.
Both the broker and listeners check the connecting effective user ID. FSKit
uses a random per-mount endpoint name and the same transport, with no TCP or
Unix socket fallback.

## Development setup

Run from the repository:

```sh
crates/runtime/polymer-ipc/macos/install-broker.sh
```

This builds the Rust broker and registers
`~/Library/LaunchAgents/org.polymer.ipc.plist`, pointing at this checkout's debug
binary. Running it again restarts the broker, so restart listeners afterward.
Remove the registration with:

```sh
launchctl bootout "gui/$(id -u)/org.polymer.ipc"
rm "$HOME/Library/LaunchAgents/org.polymer.ipc.plist"
```

A distributable macOS app must package and register the helper instead of using
this development path. Sandboxed clients need permission to look up the
`org.polymer.ipc` Mach service; the FSKit extension includes that entitlement.

Most code uses objc2. `src/xpc/protocol.m` supplies clang's static extended
protocol metadata, which NSXPC requires, and three typed proxy sends. Those
sends avoid objc2's debug method-table check on dynamically forwarded XPC
selectors. Exported objects, reply blocks, connection lifetimes, and resource
handling are Rust.

## Verification

With the broker registered:

```sh
cargo test -p polymer-ipc --test macos input_ring_and_feedback_cross_processes -- --exact --ignored
cargo test -p polymer-ipc --test macos metal_resources_cross_processes -- --exact --ignored
cargo test -p polymer-ipc --test macos listener_close_releases_registration_and_wakes_accept -- --exact --ignored
cargo check -p polymer-ipc --features input-host
cargo check -p polymer-ipc --features input-host --target x86_64-pc-windows-msvc
```

The tests launch separate processes and exercise shared input mappings, control
feedback, disconnect wakeup, IOSurface pixels, and GPU signaling of both Metal
timelines. The ignored `*_peer` entries are subprocess entry points; run the
parent tests above individually. See the [FSKit host](../../filesystem/polymer-vfs-fskit/README.md)
for the signed live mount test.
