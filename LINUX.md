# Polymer on Linux

A feasibility investigation, companion to [MACOS.md](MACOS.md) and
[ARCHITECTURE.md](ARCHITECTURE.md).

> **Status: investigation; no Polymer implementation changes.** The initial assessment was
> source-only. The container experiment below now validates some launch and software-rendering
> behavior on ARM64 Linux under Apple's `container`. **[tested]** means observed in that lab;
> **[verified]** means checked against the tree or upstream sources; **[reasoned]** remains a
> hypothesis. Hardware rendering, dma-buf sharing and gamescope capture remain unvalidated.

## Container experiment (2026-09-19)

**Dolphin's Flatpak runs against a private XWayland display. Stock gamescope cannot initialize
with this container's software Vulkan device.** The latter is an environment limitation, not
evidence that gamescope cannot host Dolphin on GPU-equipped Linux. **[tested]**

### Environment and evidence

The retained container is `polymer-linux-lab`, created with Apple `container` 1.4.1 on an Apple
Silicon Mac. It uses `debian:trixie-slim` (image index digest
`sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a`), four vCPUs,
4 GiB memory, 256 MiB `/dev/shm`, and `CAP_SYS_ADMIN` for nested-sandbox diagnostics. This is a
lab configuration, not a proposed production permission set. No repository or ROM directory
was mounted into it.

| Component | Observed version |
| --- | --- |
| Linux | 6.18.35, aarch64 |
| gamescope | 3.16.22+ds-1~bpo13+1, Debian trixie-backports/contrib |
| Flatpak | 1.16.6-1~deb13u2 |
| Dolphin Flatpak | 2606a, aarch64/stable, KDE Platform 6.10 |
| Dolphin commit | `50741ae7267181560aa88d3a1ba7177321629c171ad2677504409afa6b4462db` |
| Weston / XWayland | 14.0.2 / 24.1.6 |
| Mesa | 25.0.7, llvmpipe / LLVM 19.1.7 |
| XQuartz server | X.Org 21.1.23 |

Evidence is retained in `/home/lab/results` inside the container and copied to
`/tmp/polymer-linux-lab/results` on the Mac. The latter is temporary storage. The base image
download was about 29 MB, but the complete lab is not tiny: the Flatpak installation occupies
about 1.2 GiB and `/usr` about 754 MiB. The running lab reported about 1.86 GiB memory usage,
including its software rendering workload/cache. **[tested]**

### Results

| Probe | Result and evidence file |
| --- | --- |
| Linux user namespaces | `unshare -Ur true` succeeds. |
| Flatpak sandbox | Initially fails mounting `/proc`; succeeds after correcting the outer container mounts. `flatpak-sandbox-unmasked.log`. |
| Dolphin directly on XQuartz | Main `Dolphin 2606a` window and first-run statistics dialog exist on the remote X display. `xquartz-window-tree.txt`, `dolphin-xquartz-retry.log`. |
| Dolphin on private XWayland | Main `Dolphin 2606a` window exists under Weston's XWM on container display `:0`. `private-window-tree.txt`, `dolphin-private-xwayland.log`. |
| OpenGL on private XWayland | GLX context reports OpenGL 4.5 / llvmpipe, explicitly **not accelerated**. `private-xwayland-glx.txt`. |
| Vulkan presentation | `vkcube --wsi xcb --c 120` exits 0 on both private XWayland and XQuartz, using the CPU Vulkan device. `vkcube-private.log`, `vkcube-xquartz.log`. |
| Explicit Flatpak preload | `--env=LD_PRELOAD=/usr/lib/aarch64-linux-gnu/libm.so.6` survives; library appears in process maps, absent in the control run. `flatpak-routing-preload.log`, `preload-control.log`. |
| Socket/path exposure | With `--unshare=network`, sandbox sees private `DISPLAY=:0`, no Wayland display, and `/tmp/.X11-unix/X0`. A write to a granted profile directory is visible outside the sandbox. `flatpak-routing-preload.log`. |
| gamescope headless | Fails before guest launch: `physical device doesn't support VK_EXT_physical_device_drm`. `gamescope-headless.log`. |
| gamescope nested on XQuartz | Same missing Vulkan extension; SDL backend subsequently aborts. `gamescope-xquartz.log`. |

Window creation was checked through X11 window-tree queries and process/log inspection. The
computer-use service timed out, so this is not a visual screenshot review. No ROM was supplied
or launched: Dolphin's emulation renderer, audio, gameplay, controllers and performance remain
untested. The cube probes validate presentation by a separate native application, not Dolphin's
graphics backend or its Flatpak runtime driver. The preload probe uses a runtime library, not a
Polymer hook. The file-write probe is an ordinary filesystem grant, not Polymer's FUSE VFS.

### Setup issues and what they mean

1. **Nested Flatpak needs suitable outer-container mounts.** The default container masks
   `/proc/keys` and `/proc/timer_list`, and makes `/proc/{bus,fs,irq,sys}` read-only. Bubblewrap
   failed with `Can't mount proc on /proc: Operation not permitted`, even with user namespaces
   and `CAP_SYS_ADMIN`. Unmounting those six overlays **inside this disposable container** fixed
   the problem. For a fresh lab, evaluate `--masked-path NONE --read-only-path NONE` instead;
   that creation-time variant was not tested here. The manual unmounts must be repeated after
   a container restart. Flatpak's own sandbox was retained.
2. **Minimal images need D-Bus plumbing.** A user session bus alone did not complete the app
   install: Flatpak's parental-control lookup failed to connect. Installing
   `dbus-system-bus-common` and starting `dbus-daemon --system --fork` resolved this. Commands
   then used `dbus-run-session`. A lab user, mode-0700 `/run/user/1000`, and mode-1777
   `/tmp/.X11-unix` were also required.
3. **XQuartz is a display transport, not a Linux GPU.** There is no `/dev/dri` in this container.
   Vulkan exposes only the CPU device. Both gamescope backends reject its missing DRM extension.
   Enabling a different XQuartz transport would not supply that extension or a DRM render node.
4. **Remote X11 has shared-memory limits.** Weston with `--backend=x11 --renderer=pixman`
   attempted MIT-SHM and failed attaching memory across the VM/Mac boundary. Its headless
   Pixman backend works. Direct Qt used `QT_X11_NO_MITSHM=1`; direct `glxinfo` against XQuartz
   failed, although Vulkan's software XCB presentation succeeded. Do not conflate these paths.
5. **The guest package already chooses X11.** Dolphin's manifest sets `QT_QPA_PLATFORM=xcb`
   and grants broad access including `devices=all`, `host:ro`, network and IPC. Successful private
   display routing therefore does not demonstrate filesystem or security isolation. The lab
   lacks desktop portals; settings/file-chooser portal warnings are expected and remain relevant
   to later UI integration.

XQuartz retained its normal local socket and authentication. A temporary Python relay at
`/tmp/polymer-linux-lab/x11-relay.py` listens only on the container bridge address
`192.168.64.1:6000`, accepts only this container's `192.168.64.2`, and forwards to the local
XQuartz socket. Xauthority was copied privately, without disabling access control. The bridge
addresses and cookie are session-specific; do not bake them into a product launcher. An earlier
direct Dolphin session lost its connection when the relay was interrupted; the retry succeeded.

### Reproducing the successful private-display path

With the installed lab packages, system bus, corrected mounts and runtime/socket directories:

```sh
container exec --user lab --env XDG_RUNTIME_DIR=/run/user/1000 -d \
  polymer-linux-lab sh -lc 'dbus-run-session -- weston \
  --backend=headless --renderer=pixman --xwayland \
  --width=960 --height=720 --socket=wayland-polymer --idle-time=0 --no-config \
  > /home/lab/results/weston-headless.log 2>&1'

# Read Weston's log for the assigned X display; this experiment received :0.
container exec --user lab --env DISPLAY=:0 --env XDG_RUNTIME_DIR=/run/user/1000 -d \
  polymer-linux-lab sh -lc 'dbus-run-session -- flatpak run \
  --socket=x11 --nosocket=wayland --env=QT_QPA_PLATFORM=xcb \
  --filesystem=/home/lab/private-profile org.DolphinEmu.dolphin-emu \
  --user=/home/lab/private-profile \
  > /home/lab/results/dolphin-private-xwayland.log 2>&1'
```

Do not start a second copy of these commands while their existing processes are running.
`container stop polymer-linux-lab` stops the lab while retaining its installed filesystem.

### Updated choice of next experiment

* **Continue locally:** use Weston headless/Pixman + XWayland to investigate Flatpak launch
  mechanics, window identity/lifecycle, configuration routes and software-compatible input.
  This is a useful test environment, not a choice of Polymer's production compositor.
* **Evaluate stock gamescope capture:** move this package/launch recipe to Linux with a supported
  Vulkan GPU and `/dev/dri`. Neither a nested XQuartz window nor headless mode removes the GPU
  interface requirement observed in gamescope 3.16.22.
* **Choose custom compositor vs gamescope fork:** still depends on independently exporting
  original per-window buffers, release synchronization and multi-screen behavior. None of those
  were proved by this lab. There is no justification yet for patching around gamescope's DRM
  checks merely to make the macOS container start.

AppImage guest execution, gamescope PipeWire export, dma-buf import and latency remain open.

## Follow-up findings (2026-09-19)

**Supporting AppImage and Flatpak emulator guests looks feasible.** The preferred research
direction is a private, accelerated XWayland display backed by a headless compositor. The
emulator can remain entirely X11; Polymer's desktop session can also remain X11. Wayland is
an internal connection between XWayland and the compositor, not a requirement placed on the
emulator or the user's desktop. **[reasoned architecture; XWayland support verified upstream]**

There are two different packaging questions: launching packaged emulators, and distributing
Polymer itself in those formats. A host-native/AppImage Polymer launching Flatpak emulators
is the simpler topology. A Flatpak Polymer orchestrating external applications introduces a
separate host-launch and filesystem problem; see the packaging matrix below.

The important corrections to the first pass are:

* Gamescope **already exports frames through PipeWire** and has a **headless backend**.
  However, its capture stream is not an API for exporting each original guest swapchain.
* A compositor offers a virtual **display**, not automatic ownership of the client's swapchain
  allocation. Zero-copy forwarding is a goal that depends on buffer import and synchronization.
* Flatpak strips inherited loader variables, but **does not categorically prohibit preload**.
  Explicit overrides and an in-sandbox wrapper remain possible.
* Private display routing is not a security sandbox, and `uinput` devices are not private to
  one guest. Neither should be described as complete isolation.

Sources for these corrections and the remaining experiments are detailed below. Upstream
links track moving branches; pin versions/commits before conducting the experiments.

## The central idea: don't hook, become the display server

On Windows Polymer hands the guest a synthetic `IDXGISwapChain3`; on macOS, a synthetic
`CAMetalDrawable`. Both are elaborate ways to get at buffers the guest would otherwise hand to the
system compositor.

**On Linux, Polymer can own the guest's display server.** Accelerated clients can submit
dma-buf-backed buffers; software clients can use shared memory instead. Polymer could consume
these without an injected graphics hook. This does not guarantee dma-buf use or importability
on every driver. **[verified protocol; reasoned Polymer integration]**
[Linux dma-buf protocol](https://raw.githubusercontent.com/wayland-mirror/wayland-protocols/main/stable/linux-dmabuf/linux-dmabuf-v1.xml).

That single decision collapses three separate problems:

* **Graphics capture** can use display-protocol buffers, potentially avoiding an intermediate copy.
  This substitutes display virtualization for swapchain interception.
* **Desktop routing** keeps guest windows on a private display. Preventing a guest from connecting
  to other displays, using portals, or activating another process requires separate policy.
* **Keyboard and mouse routing** becomes native — Polymer decides what input the guest receives
  because Polymer *is* the guest's input source.

The cost is that Polymer has to ship a compositor. That is the central trade to evaluate.

### X11 guests are covered by XWayland

The user's constraint — many emulators are X11-only — does not break this. A Wayland compositor runs
**XWayland** to host X11 clients. Its accelerated path bridges X11 rendering to Wayland buffers.
Use rootless XWayland plus an X window manager so Polymer can identify individual windows,
configure their sizes, and handle focus, popups and transient dialogs. **[verified upstream;
reasoned integration]**
[XWayland architecture](https://wayland.freedesktop.org/docs/book/Xwayland.html),
[Smithay X window manager support](https://smithay.github.io/smithay/smithay/xwayland/xwm/index.html).

This is also the answer to "where does the private desktop go": explicitly start one XWayland
instance per guest session. Sharing one XWayland server between guests would share its X11
namespace and input focus. Xvfb or Xephyr would be useful diagnostics, but should not be assumed
to provide the accelerated buffer-sharing path Polymer needs. **[reasoned]**

### gamescope

The instinct is right — gamescope is exactly this micro-compositor, already handling XWayland, scaling,
resolution override and cursor grabbing for games.

Gamescope's main license is BSD-2-Clause, with additional component notices. Vendoring is
plausible, but distribution must also account for bundled dependencies such as XWayland.
**[verified]** [Gamescope license](https://github.com/ValveSoftware/gamescope/blob/master/LICENSE).

Three ways to use it, in increasing order of effort and control:

1. **Spawn stock gamescope.** Best initial experiment: existing XWayland handling and capture.
2. **Patch gamescope with per-surface export/control.** Reuses its compatibility work, but commits
   Polymer to maintaining an integration against gamescope internals.
3. **Build a focused compositor using Smithay or wlroots.** More control over surfaces and release
   timing, but Polymer owns window-management and driver compatibility work.

#### What stock gamescope actually provides

* `--backend headless` selects a backend without a desktop window or DRM output; `-w/-h/-r`
  configure the guest display. `--ready-fd` supports startup coordination. **[verified]**
  [CLI source](https://github.com/ValveSoftware/gamescope/blob/master/src/main.cpp).
* The headless backend still initializes Vulkan. It is not a CPU-only solution, and its current
  modifier/HDR capabilities differ from other backends. **[verified]**
  [Headless backend](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/HeadlessBackend.cpp).
* With PipeWire enabled, capture negotiates dma-buf or shared-memory buffers. The inspected path
  allocates exportable capture textures, uses linear dma-bufs with a single-plane restriction,
  and has a CPU-copy fallback. **[verified]**
  [PipeWire implementation](https://github.com/ValveSoftware/gamescope/blob/master/src/pipewire.cpp).
* The private `gamescope_pipewire` protocol advertises a PipeWire node ID for the main output.
  This is a concrete discovery mechanism, but not a stable public embedding contract. Node
  discovery also does not grant PipeWire access across a sandbox. **[verified protocol;
  reasoned integration caveat]**
  [Protocol definition](https://github.com/ValveSoftware/gamescope/blob/master/protocol/gamescope-pipewire.xml).
* `paint_pipewire()` builds a scene into a capture texture and waits for GPU completion before
  queueing it. The current source also supports selecting focus by app ID, which still is not
  independent simultaneous export of every X window. **[verified source]**
  [Compositor capture path](https://github.com/ValveSoftware/gamescope/blob/master/src/steamcompmgr.cpp).

**Implication:** stock gamescope can plausibly supply one integrated game view without desktop
screen capture. It introduces a capture rendering pass; dma-buf transport does not make this
equivalent to forwarding the original guest image. Two emulator windows in one process, such as
two handheld screens, need separate buffers and identities for Polymer's recomposition. Cropping
one composite loses independent timing and may lose obscured content. `--xwayland-count` does not
split a process's existing windows into independent streams. **[reasoned]**

Use stock gamescope to establish compatibility and measure capture costs before choosing a fork
or custom compositor. Smithay is particularly worth comparing for this Rust project: it supplies
XWayland lifecycle and XWM building blocks, but leaves compositor policy to Polymer. Calling this
a "minimal compositor" should not obscure the work in resizing, focus, dialogs, buffer lifetimes,
driver fallbacks and crash recovery.
[Smithay](https://smithay.github.io/smithay/smithay/),
[XWayland module](https://smithay.github.io/smithay/smithay/xwayland/index.html).

## Packaging: separate guest support from Polymer distribution

### AppImage guests

AppImage itself is packaging rather than a sandbox. Launch its normal entry point with the
private display environment. Common type-2 images use FUSE, with extraction as a fallback;
keep the normal AppRun initialization so bundled resources resolve correctly. **[verified]**
[AppImage architecture](https://docs.appimage.org/reference/architecture.html),
[FUSE fallback](https://docs.appimage.org/user-guide/troubleshooting/fuse.html).

Two real caveats:

* **ABI compatibility.** If hooks are needed, build against a suitable old baseline and verify
  required symbols against the actual guest runtime. Compatibility is not symmetric: older
  binaries usually work with newer glibc, while newer symbol requirements fail on older glibc.
* AppImages often set their own `LD_LIBRARY_PATH` and bundle libraries; the hook must tolerate
  loading alongside a bundled SDL/Vulkan loader that differs from the host's.

The compositor path avoids loading Polymer's graphics code into that library environment.
For Polymer's own AppImage, bundled library paths must also be kept out of host `flatpak` and
other external launch tools. These are launch-environment design requirements, not validated
compatibility claims. **[reasoned]**

### Flatpak X11 guests: a concrete route

Flatpak's X11 setup reads the **launcher's `DISPLAY`**, finds `/tmp/.X11-unix/XN`, binds that
socket into the sandbox, and propagates matching Xauthority data. It does not inherently select
the user's main desktop. **[verified source]**
[Flatpak X11 setup](https://github.com/flatpak/flatpak/blob/main/common/flatpak-run-x11.c).

Proposed first experiment, **not executed**; `APP_ID` and the ROM path are placeholders:

```sh
gamescope --backend headless -w 1280 -h 720 -r 60 -- \
  flatpak run --socket=x11 --nosocket=wayland --device=dri \
  --filesystem=/absolute/polymer-session:ro \
  APP_ID /absolute/polymer-session/game.rom
```

Gamescope must start the `flatpak` launcher with its private display environment. Setting only
`flatpak run --env=DISPLAY=...` is insufficient to choose the socket Flatpak mounts. A custom
launcher would also supply the private display's authentication where applicable. Use a real
filesystem X socket; abstract sockets have network-namespace implications. **[reasoned launch
recipe based on the X11 setup above]**

The read-only mount illustrates ROM access only. Saves/configuration require separate writable
routes. Existing app permissions and user overrides still apply; this command does not establish
a fully isolated guest. Per-launch arguments avoid persistent `flatpak override` changes.
[Flatpak run reference](https://docs.flatpak.org/en/latest/flatpak-command-reference.html#flatpak-run).

Native Wayland guests could instead receive a private `WAYLAND_DISPLAY` through `--socket=wayland`.
The inspected Flatpak code preserves names beginning `wayland-` without slashes and maps the
selected socket into the sandbox. Prefer a name such as `wayland-polymer-SESSION`; do not assume
arbitrary socket paths or a synthetic `XDG_RUNTIME_DIR` survive its setup. X11 guests do not
need this route. **[verified source; reasoned naming choice]**
[Flatpak Wayland setup](https://github.com/flatpak/flatpak/blob/main/common/flatpak-run-wayland.c).

### Flatpak hooks are possible, but optional for the frame path

The earlier assertion that `--env=LD_PRELOAD=...` cannot survive was incorrect. In current
`flatpak-run.c`, `flatpak_run_apply_env_default()` clears inherited loader variables, then
`flatpak_run_apply_env_vars()` applies explicit context values. An in-sandbox wrapper can also
set `LD_PRELOAD` before executing the emulator. **[verified source ordering; not tested with
an emulator]** [Environment setup](https://github.com/flatpak/flatpak/blob/main/common/flatpak-run.c).

A hook still needs a sandbox-visible `.so`, compatible dependencies and the guest architecture.
A read-only filesystem grant or a suitable extension can supply it; replacing all of `/app` or
`/usr` is not the default solution. Runtime SDK builds are useful, but one artifact per runtime
version is not automatically necessary if their ABI requirements overlap. **[reasoned]**

Flatpak supports Vulkan-layer extensions, and a gamescope extension already exists. This is
packaging precedent, not proof of Polymer compatibility or availability in every app/runtime.
[Extension documentation](https://docs.flatpak.org/en/latest/extension.html),
[Gamescope Flatpak extension](https://github.com/flathub/org.freedesktop.Platform.VulkanLayer.gamescope).

### Distribution matrix

| Polymer package | Emulator package | Feasibility and extra work |
| --- | --- | --- |
| Native/AppImage | Native/AppImage | Direct private-display launch; bundled library environment and VFS mounting need care. |
| Native/AppImage | Flatpak | Host compositor starts `flatpak run` against its XWayland display; explicit ROM/save/config access. |
| Flatpak | Emulator bundled inside that sandbox | Compositor and emulator can share sandbox resources; creates responsibility for bundling/updating emulators. |
| Flatpak | External AppImage or another Flatpak | Needs host orchestration or a deliberately designed cross-sandbox service; not solved merely by bundling gamescope. |

These are architectural assessments, not tested support guarantees. For the last case,
`flatpak-spawn --host` is an available mechanism requiring access to the
`org.freedesktop.Flatpak` D-Bus interface. It permits broad host execution; it is not a narrow
"launch emulator" portal. A separately installed host companion could own XWayland, FUSE and
guest lifecycle, but then the Flatpak is not a self-contained installation. Host-launched code
also cannot assume the Flatpak's `/app` and `/usr` exist on the host. **[verified mechanism;
reasoned deployment tradeoff]**
[flatpak-spawn documentation](https://github.com/flatpak/flatpak/blob/main/doc/flatpak-spawn.xml).

**Recommendation:** investigate native/AppImage Polymer supporting both guest formats first.
Keep Flatpak distribution possible through an explicit host-orchestration design; do not promise
feature parity or store acceptance before that design and its permissions are evaluated.

## What the existing tree gives us

### The Vulkan layer remains an alternative

`polymer-hook-vulkan-layer` is structurally portable — the layer plumbing, dispatch tables, hook
chain and swapchain virtualisation provide reusable structure. The first pass identified the
following porting points; these are not a claim that the complete port is mechanical:

| Windows-specific in the layer | Linux replacement |
| --- | --- |
| `vkCreateWin32SurfaceKHR` (`lib.rs`, `hooks.rs`) | `vkCreateXlibSurfaceKHR` / `vkCreateXcbSurfaceKHR` / `vkCreateWaylandSurfaceKHR` |
| `vkGetMemoryWin32HandlePropertiesKHR` (`terminal.rs`) | `vkGetMemoryFdPropertiesKHR` |
| `vkImportSemaphoreWin32HandleKHR` (`terminal.rs`) | `vkImportSemaphoreFdKHR` |
| `polymer_platform_windows::FixedWindowBinding` (`swapchain/wsi.rs`) | compositor-side surface, or X11/Wayland binding |
| `ExternalMemoryHandleTypeFlags::OPAQUE_WIN32` / `D3D12_RESOURCE` | `OPAQUE_FD` / `DMA_BUF_EXT` |
| `ExternalSemaphoreHandleTypeFlags::OPAQUE_WIN32` | `OPAQUE_FD` / `SYNC_FD` |

Keep a Linux Vulkan layer as a candidate when exact swapchain interception is necessary. It
would load Polymer code into the guest through the Vulkan loader, and would not cover GLX/EGL
OpenGL guests. Flatpak needs layer files and dependencies inside its runtime view. **[reasoned]**

### Buffer sharing needs a Linux protocol and consumer

`dma-buf` fds and `sync_file` / `drm_syncobj` fds both travel over a unix socket with `SCM_RIGHTS`.
That gives a consistent transport, but does not make their synchronization semantics equivalent.
`polymer-ipc` has a `LocalSocket` abstraction, whose current control and shared-memory
implementations use Windows APIs. `polymer-consumer-vulkan` imports Windows handle types and
matches adapter LUIDs. These need actual Linux backends, not just renamed handle enums.
**[verified tree]** See `crates/runtime/polymer-ipc/src/{control,shared}.rs` and
`crates/consumer/polymer-consumer-vulkan/src/lib.rs`.

For a custom compositor, the intended frame path is **[reasoned]**:

```text
X11 emulator (GLX/EGL/Vulkan)
    -> private XWayland + Polymer XWM
    -> committed per-window buffers
    -> dma-buf + metadata + acquire/release synchronization
    -> Polymer Vulkan consumer -> Polymer GUI on X11 or Wayland
```

The contracts would need:

* A stable Polymer surface identity mapped to X window/surface lifecycle. Keep game surfaces,
  dialogs and cursor content distinct; preserve the existing plugin surface-selection intent.
* Width/height, DRM format, modifier, plane strides/offsets and fd ownership. Query import
  compatibility for the actual consumer GPU, including hybrid-GPU systems. `OPAQUE_FD` and
  `DMA_BUF_EXT` are different handle contracts.
* Acquire synchronization before sampling, and release only after **Polymer's GPU** finishes.
  Closing a transferred fd is not a release fence. Premature client-buffer reuse would corrupt
  frames; a bounded buffering policy must also prevent a slow GUI from stalling the emulator.
* A fallback upload/copy path for shared-memory or incompatible buffers, explicitly distinguished
  from the direct-import fast path.
* A scheduling policy for X Present/Wayland callbacks and Polymer display pacing. A received
  buffer is not proof it was displayed to the user; do not assume XWayland commits expose every
  internal emulator present or preserve two screens' timing automatically.

The dma-buf protocol defines format/modifier and plane metadata. Explicit-sync protocols supply
acquire/release timeline points, while older clients may rely on implicit synchronization.
Their translation into Vulkan requires capability checks, not merely passing fds. **[verified
protocols; implementation remains open]**
[dma-buf](https://raw.githubusercontent.com/wayland-mirror/wayland-protocols/main/stable/linux-dmabuf/linux-dmabuf-v1.xml),
[DRM syncobj](https://raw.githubusercontent.com/wayland-mirror/wayland-protocols/main/staging/linux-drm-syncobj/linux-drm-syncobj-v1.xml).

The current `polymer-platform::Guest` trait is a useful boundary for process lifecycle, window
input and audio. Its launch request assumes an executable in a mandatory VFS mount. An installed
Flatpak app ID plus runtime is a different launch shape; preserve the filesystem contract while
designing that mapping, rather than treating `flatpak` as just an emulator executable.
**[verified tree; reasoned integration]** `crates/runtime/polymer-platform/src/lib.rs`.

### Modal dialogs come from accessibility events

Guest message boxes are detected by `polymer-automation` from xa11y window-open events rather than
by hooking toolkit dialog APIs, so Linux support follows from an AT-SPI xa11y backend.

## Input: compositor routing plus gamepad policy

Keyboard, pointer, relative motion and focus belong in the private compositor/XWM. A headless
gamescope capture stream supplies no input channel by itself. For an X11-only prototype, evaluate
XTEST plus explicit focus control on the private X server; production still needs relative mouse,
touch coordinates and grabs validated. A custom compositor can expose Polymer-controlled seat
input. **[reasoned; gamescope control integration remains open]**

`uinput` creates kernel input devices, usable by applications that can access their event nodes.
Force feedback is a separate bidirectional protocol; advertising rumble capability is not enough
to relay effect upload/playback to Polymer's input hub. **[verified]**
[uinput](https://docs.kernel.org/input/uinput.html),
[force feedback](https://docs.kernel.org/input/ff.html).

Consequences to investigate **[reasoned]**:

* Virtual devices can be visible to other applications and to Polymer itself. Avoid reading them
  back into the host input hub, exposing duplicate physical controllers, or routing one guest's
  pad into another guest. A private X server does not isolate gamepads.
* Stable identity, hotplug ordering, four-player assignment, rumble and richer controller features
  need testing. SDL hooks may remain useful for per-process visibility and custom capabilities.
* Prefer creating virtual pads in the host service and exposing the resulting event devices to
  the guest. Permission to read `/dev/input` does not imply permission to create `/dev/uinput`
  devices. Flatpak permissions cannot override host device-node ownership.

Flatpak now documents `--device=input`, with compatibility handling for older versions. Select a
minimum version rather than assuming it is universally available; avoid assuming the USB portal
solves arbitrary raw HID, uinput or Bluetooth transport access.
[Device permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html).

The first pass proposed Linux HCI user-channel or raw USB transports for Polymer's Bluetooth
stack. Keep that as a separate investigation: adapter ownership, BlueZ coexistence, privileges
and Flatpak syscall/device restrictions are unresolved. It is not required to prove the frame path.

## Other subsystems

| Windows | Linux | Notes |
| --- | --- | --- |
| `polymer-vfs-winfsp` | FUSE | Prefer mounting on host, then exposing selected routes to guest; device access alone does not establish mount permissions. |
| DirectComposition / `Windows.UI.Composition` | Polymer's own scene graph | As the compositor, there is no system API to map onto — composite in the renderer |
| `PrivateDesktop` (`desktop.rs`) | private compositor + XWayland | Display isolation, not a complete sandbox. |
| Job objects (`KillOnCloseJob`) | supervised scope/cgroup and process tracking | Parent-death signals/process groups alone are not equivalent tree containment; track Flatpak instance lifetime. |
| `QueryPerformanceCounter` | `clock_gettime(CLOCK_MONOTONIC)` | |
| Media Foundation H.264 | VA-API / NVENC | |
| Per-process audio (`ISimpleAudioVolume`) | PipeWire per-node volume | Associate all guest streams across sandbox PID namespaces; handle node recreation and session policy. |
| UI Automation | AT-SPI2 | Emulator/toolkit coverage and sandbox bus access need testing. |
| Registry-based emulator discovery | `.desktop` files, Flatpak/AppImage manifests | |

These remain candidate mappings, not validated Linux implementations. FUSE must be mounted
before the Flatpak launch binds its paths. Confirm save/config redirection, mounts across
namespaces, concurrent instances of one app ID, flush completion, and teardown before claiming
the existing mandatory VFS behavior is preserved. **[reasoned]**

## Decision criteria and validation order

The original source-only validation plan follows. The container experiment above now covers
part of the launch path and software rendering; hardware and full Polymer integration remain:

| Experiment | Evidence needed / decision unlocked |
| --- | --- |
| Stock headless gamescope + X11 emulator, both AppImage and Flatpak | Private display selected; hardware GLX/EGL and Vulkan render; no visible guest desktop window. Record exact gamescope/XWayland/Flatpak/driver versions. |
| PipeWire consumer on the same host | Discover the correct node, negotiate dma-buf, import into the intended GPU, measure capture passes and latency; identify any shared-memory fallback. |
| Two game windows plus a dialog | Determine which surfaces the stock stream loses or combines. Require independent identity, resolution and updates for Polymer's full multi-screen behavior. |
| Small Smithay/wlroots experiment or gamescope export patch | Forward two independent buffers with correct acquire/release lifetime, startup resize policy and no starvation. Compare effort against maintaining a fork. |
| Input and lifecycle | Hotkeys, pointer grabs, touch mapping, pause/resume, virtual-pad assignment/rumble, crashes and full process-tree cleanup. |
| VFS + Flatpak | Guest sees only intended game/config/save routes; writes reach Polymer storage; relaunch and concurrent instances do not corrupt state. |
| Driver/package matrix | Intel/AMD Mesa, NVIDIA proprietary, hybrid GPUs; host X11 first, then Wayland; oldest supported runtime and Flatpak. |
| Polymer packaged as Flatpak | Prove host companion/spawn deployment, cross-boundary frame access and updates without assuming host access to sandbox binaries. |

If stock gamescope's single capture stream and measured latency suffice for a reduced first
milestone, it avoids owning a compositor immediately. If independent original surfaces are
required from the outset, compare a per-surface gamescope extension against a focused Rust
compositor. Keep a Vulkan layer available for requirements that a display server cannot express.

Containers or VMs can validate launch logic and software rendering; GPU-enabled environments can
also exercise some sharing paths. Final compatibility and latency claims require representative
Linux GPU drivers/hardware. A headless compositor need not own a physical display or DRM master.
