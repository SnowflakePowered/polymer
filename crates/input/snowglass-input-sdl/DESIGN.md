# SDL input provider architecture

The SDL provider is split into one version-neutral state crate and two ABI-only
hook DLLs:

```text
Message::GamepadUpdate
          |
          v
 snowglass-input-sdl
 Runtime / sequencing / fixed slots / SdlBackend
          |
          +-----------------------------+
          |                             |
 snowglass-hook-sdl2.dll       snowglass-hook-sdl3.dll
 SDL2 indices and exports      SDL3 IDs, descriptors, and exports
```

`snowglass-input-sdl` owns no SDL ABI declarations and no hook installation.
It receives complete sequenced snapshots, rejects stale updates, neutralizes a
disconnected fixed slot, and asks an `SdlBackend` to attach or publish.

Each hook DLL independently discovers only its SDL major version, installs its
enumeration and update detours, implements `SdlBackend`, and owns the resulting
SDL device identities. SDL2 indices can therefore never cross into SDL3's
instance-ID model. SDL3 intercepts both `SDL_GetJoysticks` and
`SDL_GetGamepads`, since an emulator may use only the higher-level enumerator.

The enumerator detour lazily creates the configured virtual devices before the
real enumeration. The update detour publishes dirty snapshots before SDL's real
update pass, leaving SDL responsible for public handles, mappings, polling, and
events.

Topology remains fixed at 1-4 pads through `SNOWGLASS_SDL_GAMEPADS` (default 1).
Dynamic hotplug, rumble/LED feedback, and viewer-side controller capture remain
separate follow-up work.
