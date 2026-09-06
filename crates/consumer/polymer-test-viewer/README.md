# Capture test viewers

Viewer-side IPC, native imports, and pixel validation for graphics capture
fixtures live here, outside the producer implementations. `WsiViewer` is shared
fixture support; the `polymer-test-viewer` executable runs opaque Vulkan imports
in a separate process with capture-layer activation removed.

Build the executable before running an opaque WSI fixture:

```powershell
cargo build -p polymer-test-viewer
```

The fixture locates the executable beside its profile's `deps` directory. Set
`POLYMER_WSI_OPAQUE=1` alongside the existing WSI viewer and hook environment to
exercise opaque Vulkan imports. The producer forwards borrowed export handles
through the common producer session, which duplicates them into the child.
The child imports, verifies pixels, signals consumer release, and exits.

The Vulkan producer's native graphics and external-application fixtures remain
in that producer's tests; they depend on this crate only as test support.
