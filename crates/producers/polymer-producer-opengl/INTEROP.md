# NV interop texture investigation

Investigated on 2026-09-07 using an NVIDIA GeForce RTX 5070 Ti, driver
32.0.16.1062. The existing NV path blits into a private WGL-registered D3D11
texture, unlocks it, and copies it into the exported NT-handle keyed-mutex slot.

A separate hidden-window probe created RGBA8, single-sample D3D11 textures
with render-target and shader-resource bindings. It registered each texture
using `wglDXRegisterObjectNV`, then attempted 16 alternating red/green frames.
For shared textures, a second D3D11 device opened the handle and checked the
pixels through a staging readback. This was a two-device, same-process probe;
it did not validate cross-process behavior or other GPU drivers.

| Texture flags | Result |
| --- | --- |
| No sharing flags (existing private intermediate) | Registration, lock, rendering and unlock succeeded for 16 frames |
| `SHARED_NTHANDLE \| SHARED_KEYEDMUTEX` | Registration failed; `GetLastError = 0xC007006E` |
| `SHARED_NTHANDLE \| SHARED` | Registration failed with the same error |
| `SHARED` (legacy handle) | Registration succeeded; second-device readback verified all 16 frames |

The registration failures occur before WGL locking, so changing keyed-mutex
acquisition order cannot resolve the observed failure. Removing the keyed
mutex alone also did not help. Keep the private intermediate for the current
NT-handle transport. The legacy result motivates a separate transport
experiment, not switching the flags on the current exported images: handle
import and cross-process synchronization would also need to change.

The EXT external-memory path already avoids this intermediate and remains
the preferred path when its extensions are available.

References:

- [WGL_NV_DX_interop](https://registry.khronos.org/OpenGL/extensions/NV/WGL_NV_DX_interop.txt): registration and lock/unlock contract.
- [WGL_NV_DX_interop2](https://registry.khronos.org/OpenGL/extensions/NV/WGL_NV_DX_interop2.txt): D3D11 resource support and restrictions. Its support for D3D11 textures is not evidence that every later sharing-flag combination works on every driver.

The local probe source and logs are in `.smoke-artifacts/ipc-texture-sharing/`.
