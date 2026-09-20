# SDL3 controller hook

The shared dynamic API table hooks expose Polymer virtual controllers using
`polymer-hook-input-sdl` and the existing input IPC session. Windows installs
entry/export detours; macOS supplies `SDL_DYNAPI_entry` through
`SDL3_DYNAMIC_API`. Both select their terminal backend with `cfg_attr` on the
`detour` attribute.

On macOS the guest launcher sets:

- `SDL3_DYNAMIC_API` to `libpolymer_hook_sdl3.dylib`.
- `POLYMER_SDL3_RUNTIME` to the backing `libSDL3.dylib`.
- `POLYMER_SESSION` to the existing input session.

The backing runtime must provide at least the guest's SDL dynamic API table
version and size. The GUI packaging script includes its SDL runtime and hook;
for unpackaged development launches, place both dylibs in the guest artifact
directory. The backing library remains loaded for the life of the hooks.

The native integration test compiles a real SDL guest, publishes controller
state through XPC, and verifies rumble feedback in the host:

```sh
cargo build -p polymer-hook-sdl3
POLYMER_SDL3_RUNTIME=/absolute/path/to/libSDL3.dylib \
  cargo test -p polymer-hook-sdl3 --test macos -- --ignored --nocapture
```

This requires SDL3 headers/pkg-config and the registered `gg.polymer.ipc` broker.
