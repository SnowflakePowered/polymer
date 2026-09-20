# Native macOS session components

The initial implementation provides:

- `StagedBundle`: a cached APFS clone, quarantine removal, entitlement-preserving
  ad-hoc signing with hardened runtime, dyld insertion permission and disabled
  library validation, followed by signature verification on cache misses. Copies
  persist in `~/Library/Caches/gg.polymer/signed-bundles`. The cache key covers
  source file contents, paths, permissions, symlink targets and signing policy;
  unchanged bundles reuse the same signed files without invoking `codesign`.
  Publication is atomic and locked across processes. Previous versions remain
  available to running guests. The cache can be removed when no guests are running.
  The source is unchanged.
  The staged bundle must be mounted into the guest VFS before direct execution.
- `SystemAudioState`: default system output volume and mute through
  `objc2-core-audio`. Each operation resolves the current default device; controls
  use the main element or the available preferred stereo channels. Fixed-volume
  devices return an unsupported/error result instead of simulating volume changes.
- `PrivateSpace`: main-thread-owned, dynamically loaded private SkyLight bindings.
  The guest owns the unmanaged Space, with destruction on drop and WindowServer
  cleanup on process death. This isolates presentation/focus, not OS input.

`polymer-hook-appkit` activates isolation when `POLYMER_DESKTOP_ISOLATION=1`.
It keeps windows ordered in, moves them onto the private Space, blocks app
activation, and sweeps visible windows after AppKit updates. It does not intercept
modal dialogs. There is no host-facing isolation-ready handshake.

`GuestPlatform` launches the executable from the mandatory FSKit mount, injects
Metal/AppKit hooks, supervises the process group, and routes native window and
input messages. SDL controller injection is not implemented yet; nonempty input
hook requests are rejected.

Plugin imports of `.app` directories use `BundleFactory` to stage only when the
launch filesystem is materialized. The filesystem lease references the cached
bundle, which survives teardown. Framework aliases resolve within that read-only tree.
The GUI development packaging script is documented in
[`bins/polymer-gui/macos`](../../../../bins/polymer-gui/macos/README.md).
