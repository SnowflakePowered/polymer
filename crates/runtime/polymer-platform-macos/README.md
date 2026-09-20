# Native macOS session components

The initial implementation provides:

- `StagedBundle`: a fresh APFS clone, quarantine removal, entitlement-preserving
  ad-hoc signing with hardened runtime, dyld insertion permission and disabled
  library validation, followed by signature verification. The source is unchanged.
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

These are native components, not a complete macOS `Guest` implementation yet.
Runtime launch wiring still needs macOS input IPC and process supervision;
Metal capture/consumption and the macOS input HAL remain separate porting work.
The FSKit host is in `polymer-vfs-fskit` and remains mandatory for guest launch.
