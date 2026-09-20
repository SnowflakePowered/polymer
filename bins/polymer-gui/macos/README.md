# macOS development app

Build with Xcode 27 selected in `DEVELOPER_DIR`:

```sh
bins/polymer-gui/macos/build.sh SIGNING_IDENTITY HOST_PROFILE EXTENSION_PROFILE OUTPUT_DIRECTORY
```

Use a fresh output directory. The profiles must authorize `gg.polymer` with
FSKit mount access and `gg.polymer.filesystem` with FSKit module access, on the
same team. The script embeds the extension, SDL, Metal/AppKit/SDL3 hooks and profiles,
signs nested code and the GUI, and verifies the resulting `Polymer.app`.
The host includes V8's executable-memory entitlements.

This development bundle uses the checkout's plugins and the existing per-user
XPC broker. Register the broker using
`crates/runtime/polymer-ipc/macos/install-broker.sh` and enable the signed
PolymerFilesystem extension before launching. Run
`OUTPUT_DIRECTORY/Polymer.app/Contents/MacOS/polymer-gui --debug` to use Horizon's
development server and expose the inspector.

Dolphin imports `osIdentifier` from `polymer:sys` to select its package. On macOS,
import the `.app` directory itself: launch preparation caches a locally re-signed
copy, rebuilding it when the source changes, then exposes it through the mandatory VFS. Internal framework
aliases resolve within that read-only bundle; the source remains unchanged.
macOS Dolphin requests Metal capture and SDL3 virtual controllers with desktop
isolation enabled. The SDL hook reuses the bundled SDL runtime through the
existing dynamic API table hooks.
