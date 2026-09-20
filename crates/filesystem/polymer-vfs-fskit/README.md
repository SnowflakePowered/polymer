# Polymer FSKit host

`FsKitFilesystemHost` mounts the existing `PreparedFilesystem` through FSKit.
The GUI and debug viewer select it on macOS. Every endpoint has its own
`/Volumes/Polymer-<nonce>` root; commands outside that root fail validation.
Desktop isolation does not control filesystem mounting.

The host retains the live VFS and storage leases. The FSKit app extension runs
Rust `objc2` implementations of `FSUnaryFileSystem` and `FSVolume`; only the
Swift-only `UnaryFileSystemExtension` entry point is Swift. File operations
cross a bounded NSXPC connection directly to the host. The per-user
`gg.polymer.ipc` broker transfers the anonymous listener endpoint; VFS traffic
does not pass through the broker. A random per-mount capability in the FSKit
resource URL selects the endpoint, and listeners accept only the same user. Names remain
byte strings, and permissions and mutations are enforced by `polymer-vfs`.
The host unmounts before stopping the server and finalizing storage.

## Build and package

Register the [IPC broker](../../runtime/polymer-ipc/README.md) before mounting.
The sandboxed extension has a Mach lookup exception for `gg.polymer.ipc`;
network client access is no longer needed.

Run `extension/build.sh` from any directory. It builds
`target/macos/PolymerFilesystem.appex` for Apple Silicon, macOS 27. `SDKROOT`
can select the SDK. This is a build step, not an installer or signer.

Embed the extension in the containing app's `Contents/Extensions`. Sign the
extension with `extension/Extension.entitlements` and the host with the
additional entitlement in `extension/Host.entitlements`, using provisioning
profiles that authorize the respective FSKit entitlements. Preserve any other
host entitlements when merging these. The extension identifier is
`gg.polymer.filesystem`; the host passes that identifier to FSKit. Register the
containing app normally and enable the extension in System Settings.

An ad-hoc signature is insufficient: FSKit requires provisioned entitlements.
Use Xcode 27 or newer for automatic provisioning of both bundle identifiers;
Xcode 26 rejects the host's new `com.apple.developer.fskit.mount` entitlement.
The standalone build uses `_NSExtensionMain`, which initializes ExtensionKit
before calling the Swift entry point.

## Verification

`cargo test -p polymer-vfs-fskit` compiles the mounted-filesystem integration
test. Package it with explicit signing inputs (use a fresh output directory):

```sh
extension/build-test-host.sh "$SIGNING_IDENTITY" "$HOST_PROFILE" "$EXTENSION_PROFILE"
```

Set `DEVELOPER_DIR` to select Xcode 27 if needed. The script produces
`target/macos/verification/PolymerFSKitTest.app` and verifies its signatures;
it does not install or enable the extension. Install the containing app, enable
PolymerFilesystem in System Settings → General → Login Items & Extensions →
File System Extensions, then run its `Contents/MacOS/PolymerFSKitTest` executable
with `--ignored --nocapture`.

The signed test has passed through XPC on macOS 27: mounting, directory/file creation,
writes, synchronization, rename while open, reads verified directly against
the live VFS, deletion, enumeration, and removal of the mount point. Unmount
uses FileManager/Disk Arbitration because direct `unmount(2)` returns EPERM
for mounts owned by the FSKit service.

The first adapter exposes the VFS's files and directories, read/write access,
metadata size/readonly changes, creation, removal, rename/replacement, and
synchronization. Symlinks, hard links, extended attributes, and arbitrary POSIX
ownership/mode changes are not advertised. Native FSKit performance and full
guest execution still need validation.
