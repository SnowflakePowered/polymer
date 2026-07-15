# Snowglass

Snowglass is the successor to snowflake-ingame.

It is a complete solution for gamescope-like recomposition of games and emulator swapchains,
allowing swapchain effects,

## Vulkan viewer test

Build the Vulkan layer and viewer, then launch a target after `--`:

```powershell
cargo build -p snowglass-hook-vulkan-layer -p snowglass-viewer
.\target\debug\snowglass-viewer.exe -- D:\emulators\dolphin\Dolphin-x64\Dolphin.exe -b -e D:\emulators\dolphin\Dolphin-x64\melee.iso
```

The viewer creates a local socket, launches the target with the Snowglass Vulkan
layer enabled, hides the target's top-level windows, imports its external image
ring and timeline semaphore, and displays captured frames with live statistics.
The direct compositor waits on the imported timeline, blits the selected image
to the viewer swapchain, renders the egui overlay, and presents it in one Vulkan
submission. Captured frame pixels never pass through CPU memory.
