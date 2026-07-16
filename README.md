# Snowglass

Snowglass is the successor to snowflake-ingame.

It is a complete solution for gamescope-like recomposition of games and emulator swapchains,
allowing swapchain effects,

## Vulkan viewer test

Build the Vulkan layer and viewer, then launch a target after `--`:

```powershell
cargo build -p snowglass-hook-vulkan-layer -p snowglass-hook-dinput -p snowglass-viewer
.\target\debug\snowglass-viewer.exe -- D:\emulators\dolphin\Dolphin-x64\Dolphin.exe -b -e D:\emulators\dolphin\Dolphin-x64\melee.iso
```

To capture a transfer-based presentation source before it is copied or blitted
to the swapchain, enable render-target tracking:

```powershell
.\target\debug\snowglass-viewer.exe --track-render-target -- <program> [arguments...]
```

Tracking is conservative: when no presentation source is recognized, Snowglass
falls back to capturing the complete swapchain. The option is disabled by
default and does not affect the normal capture path.

The viewer creates a local socket, launches the target with the Snowglass Vulkan
layer enabled, hides the target's top-level windows, imports its external image
ring and timeline semaphore, and displays captured frames with live statistics.
The direct compositor waits on the imported timeline, blits the selected image
to the viewer swapchain, renders the egui overlay, and presents it in one Vulkan
submission. Captured frame pixels never pass through CPU memory.

The Vulkan layer loads the sibling `snowglass_hook_dinput.dll`, which detours
the system `DirectInput8Create` entry point in-process. Keyboard and mouse
transitions are sent as buffered DirectInput device-data records over a
dedicated local socket and returned by intercepted
`IDirectInputDevice8::GetDeviceData` calls. No DLL is installed in the target
directory.
