# Virtual swapchains: implementation status

This records the refined acknowledgment/DDI design. It replaces the historical submission-census/software-replay progress log. The user removed the standalone probe; it has not been recreated.

## Production path: swapchain only (2026-09-08)

D3D11, D3D12, and Vulkan now support only native zero-copy swapchain presentation. Their graph trackers, draw/resource observers, copy-capture engines, native-swapchain capture ownership, and related hook tables are deleted. There is no capture fallback. Unmatched application UI surfaces remain native and unexported.

`renderMode` defaults to `"swapchain"`; every requested surface requires a fixed positive `resolution`. Pluginhost and runtime reject `"intercept"` and `"graphTracking"` with these three APIs, including mixed launch requests with capture-only backends. Hook installation independently requires a valid swapchain configuration. The old environment-only render-mode fallback and `POLYMER_GRAPH_TRACKING`/`POLYMER_PRESENT_UNTHROTTLED` controls are removed.

OpenGL retains explicit `"intercept"`/`"graphTracking"` modes, and GDI retains explicit `"intercept"`. Shared allocation utilities needed by those producers remain. D3D11's scoped DDI resource-identity collection also remains: it enables native allocation rotation and does not capture application rendering.

The debug viewer's automatic hook set is D3D11/D3D12/Vulkan. Command-line launches require `--swapchain-resolution WIDTH HEIGHT`; the interactive launcher offers editable dimensions. Graph tracking and keyed-mutex options are limited to the capture backends. Built-in plugin declarations now provide fixed output extents: DuckStation/PCSX2/RetroArch 640x480, Flycast 960x720, and Cemu Vulkan 1280x720 TV plus 854x480 GamePad. Dolphin retains its configured extent. These declarations were migrated; this change does not claim a new gameplay validation of every plugin.

The human-maintained ARCHITECTURE.md still describes graph-based capture generally. That description no longer applies to D3D11, D3D12, or Vulkan; it was left unchanged as instructed.

### Swapchain-only verification

- Runtime launch validation (17 tests), pluginhost runtime (16), debug-viewer CLI (27), shared presentation (41), and hook framework (15) passed. Horizon TypeScript checking passed, as did GUI/debug-viewer and OpenGL/GDI hook checks.
- All four Vulkan native-WSI GPU configurations passed with zero Khronos validation errors. All five existing renderer GPU regressions passed.
- Rebuilt DXGI/D3D11/D3D12/Vulkan hooks and the debug viewer. Dolphin application runs passed for each producer with both consumers, using fixed 640x528 RGB10A2 buffers and shared fences. Each successful run advanced thousands of publications and exited normally. The Vulkan/D3D12 run also underwent repeated application swapchain recreation before settling on a stable generation; no graphics errors were logged.
- The first D3D12 run exposed an old RGBA8/BGRA8-only format restriction. RGB10A2 now passes the existing device format-capability check and uses the same native shared-resource path. Both subsequent consumer runs passed; the animated output was visually checked with D3D12.
- Evidence is in `target/swapchain-only-validation/`, including separate initial-failure logs, successful hook/viewer logs, and `d3d12-frame.png`. Viewer logs contain unrelated pointer-routing warnings; input/controller behavior was not part of this verification. Existing unrelated compiler warnings remain.

## Shared IPC and ownership

Protocol version 18 adds opt-in RequestBufferAvailability and cumulative BufferAvailable messages. The stream and accepted connection identify the publication sequence and immutable resource generation.

ProducerSession polls complete control packets without consuming fragments, rejects foreign/future acknowledgments, and keeps availability monotonic. SwapchainController requires acknowledgment before recycling a published slot. Native backend fence completion and acquire synchronization remain mandatory.

Both compositors acknowledge after submitting consumer release. Retained frames release older publications; cached frames release their own publication after their copy/read is ordered. Runtime BufferReleaseSender endpoints are connection-bound and revoked on teardown. OpenGL/GDI capture producers do not subscribe.

## D3D12

The backend owns fixed native indexed shared resources and shared ready/release fences. Creation acquires buffer zero; Present publishes the current slot and acquires the next before returning successfully.

Removed: virtual resource census, shader/descriptor access tracking, submission routing/tombstones, queue bridges, private submissions, reconstructed resource states, and per-image release command allocators/lists. The previous graph-tracking and interception-capture paths have also been removed.

Applications order contributing work on the presentation queue and present in PRESENT/COMMON. Polymer does not reconstruct or repair application synchronization.

## D3D11 hook/producer separation

All DDI implementation is in crates/hooks/polymer-hook-d3d11/src/ddi/: installed-driver discovery, OpenAdapter10_2 and adapter/device interception, scoped driver-resource capture, relocation handling, registry cleanup, and native identity exchange.

The producer defines RotationProvider, RotationCapture, and ResourceRotation interfaces. It owns the physical-slot permutation and fence handoff. It contains no UMD loading, DDI table patching, or private runtime COM offsets.

GetBuffer(0) returns a native texture. The driver exchanges its allocation with the acquired destination. Native views, bindings, and command lists remain native. All logical texture/RTV wrappers, deferred recorders, software replay, and compiled per-slot recordings have been removed.

Virtual D3D11 uses shared fences for both consumers. Vulkan imports the existing D3d11D3d12Resource profile as D3D11_TEXTURE external memory. OpenGL/GDI keyed-mutex paths remain.

Factory creation is registered with shared DXGI dispatch. D3D11CreateDeviceAndSwapChain routes through the same virtual factory boundary. ID3D11Multithread protection serializes raw rotation and native ownership operations.

## Lifecycle and cleanup

Presentation owns the shared DXGI maintenance worker and frame-latency event implementation. Retirement retains outstanding resources conservatively; disconnect terminates a generation rather than replacing cached objects.

The probe-only generic FramePublisher abstraction, final-state cancellation hook, and unused ownership fields were removed. Controllers use ProducerSession directly.

Configuration remains immutable-resolution and uses configured logical identities plus HWND lifetime tracking. ARCHITECTURE.md is unchanged.

## Validation

Validation for this refinement:

- 134 unit tests passed across IPC (11), presentation (41), D3D11 producer (2), D3D12 producer (13), renderer (52), and runtime (15). Eight opt-in renderer tests were excluded from this run.
- All five existing renderer GPU regressions passed separately: D3D12 retained/cache behavior, Vulkan retained/cache behavior, and keyed-mutex redraw/skipped-frame behavior.
- The IPC regressions cover fragmented acknowledgments, monotonic duplicate handling, and rejection of foreign/future publications.
- cargo check passed for both D3D hooks, the GUI, debug viewer, Vulkan producer, and OpenGL producer, including their affected dependencies. Existing unrelated warnings remain.
- Formatting and tracked whitespace checks passed.

These existing GPU regressions validate consumer ownership behavior; they do not exercise production DDI rotation or the new D3D11 fence-only Vulkan import.

### Dolphin / debug-viewer production DDI validation (2026-09-07)

Dolphin's D3D backend running the local Melee image passed through polymer-debug-viewer on an NVIDIA GeForce RTX 5070 Ti, Windows driver 32.0.16.1062:

- D3D12 consumer: over 5,700 frames, animated intro/title output, approximately 59.9 FPS.
- Vulkan consumer: over 5,300 frames, animated intro/title output, approximately 59.9 FPS.
- Both imported the configured 640x528, three-slot RGB10A2 UNORM stream as D3d11D3d12Resource/D3d12Fence. Continued ring reuse exercises production DDI rotation, cumulative availability acknowledgments, and native release-fence checks.
- Closing each viewer terminated its Dolphin child; both exit codes were zero. No stream/import/rotation errors appeared in the successful runs.
- Tests used --no-isolate and a separate Dolphin user directory with a copy of the existing Melee memory-card save. They did not change the installed Dolphin profile. Input/controller gameplay, isolated desktop behavior, deferred command lists, and other vendors were not tested.

Validation uncovered and fixed three integration gaps: the viewer lacked a virtual-mode launch option, the runtime still rejected all virtual application launches, and the D3D11 format allowlist rejected Dolphin's RGB10A2 buffers. The runtime now permits D3D11/D3D12 virtual launches while rejecting unimplemented graphics backends. The existing transport/import formats already supported RGB10A2.

Reproduce with the built hooks beside the viewer:

```powershell
./target/debug/polymer-debug-viewer.exe --api d3d12 --hook d3d11 --no-isolate --swapchain-resolution 640 528 -- D:/emulators/dolphin/Dolphin-x64/Dolphin.exe -b -v D3D -u D:/coding/polymer/target/dolphin-ddi-validation/user -C Dolphin.Interface.SkipNKitWarning=True -e D:/emulators/dolphin/Dolphin-x64/melee.iso
```

Use --api vulkan for the second consumer. With no --track-render-targets selection, the new option requests fixed surface zero; explicit selections retain their indices and patterns.

Local evidence is in target/dolphin-ddi-validation/: d3d12-validated-hook.log, vulkan-validated-hook.log, both viewer stdout logs, and the animation PNGs. These are validation artifacts, not a recreated standalone probe.

All 34 debug-viewer tests and 16 runtime tests passed, including CLI extent/conflict validation and rejection of unsupported virtual launch backends.

## Vulkan virtual WSI implementation (2026-09-07)

Implemented in the Vulkan hook and producer `swapchain` modules. Applications receive real native VkImages backed by shared D3D12 allocations. Application command buffers, views, pipelines, and draws remain native. There is no command recorder or replay. The hook translates PRESENT layouts and virtual queue-family transfers at API boundaries, including legacy/synchronization2 barriers and render-pass/render-pass2 paths with imageless framebuffers.

The hook owns dispatch, Win32 surface matching, synthetic swapchain handles, capability enumeration, feature negotiation, and native/virtual present routing. The Vulkan producer owns images and external GPU ownership. Shared presentation owns availability, publication, pacing, and retirement. No Vulkan capture tracking remains.

Acquire supports multiple outstanding images, timeout/poll behavior, binary semaphore/fence signaling, and both acquire entry points. Present handles out-of-order indices and mixed native/virtual batches, consumes application waits once, and orders transport signals on the presenting queue. Native present fences establish completion before semaphore recycling. Replacement closes old acquisitions while allowing already-acquired images to finish; uncertain GPU completion conservatively retains resources.

### Supported profile and limits

- Fixed-resolution Win32 surfaces, 2–4 images, one array layer; FIFO and IMMEDIATE. FIFO uses a nominal 60 Hz virtual presentation cadence and waits for GPU completion before publication. The host compositor can sample/coalesce published frames.
- Capability-filtered BGRA8/RGBA8 UNORM, RGB10A2 UNORM, and RGBA16 float with SRGB_NONLINEAR; color-attachment, sampled, and transfer usage. Backing images preserve the requested usage exactly.
- D3D12_RESOURCE/D3D12_FENCE transport for both Vulkan and D3D12 consumers. Requires compatible Win32 external memory/semaphore support, timeline semaphores, synchronization2, foreign queue ownership, and internal swapchain-maintenance present fences. Feature negotiation preserves explicit application feature values and rejects unavailable requirements.
- Multiple created unprotected queue families use concurrent physical images; single-family devices use exclusive images. Application ownership barriers retain their access/layout effects.
- Optional public WSI extensions outside this profile are filtered. Swapchain image aliases and more than eight PRESENT attachments in a render pass remain software limitations. Broader WSI profiles are unfinished, not classified as unsupported hardware.

### Validation

- Presentation, D3D11/D3D12/Vulkan producer, Vulkan hook, and runtime unit suites passed.
- The opt-in `native_wsi_round_trip` test passed all four child configurations: Vulkan/D3D12 consumers with concurrent-family IMMEDIATE and exclusive-family FIFO profiles. Khronos validation reported zero errors. Coverage includes exhaustion/timeouts, reverse presentation, actual consumer pixel readback, binary waits, mixed batches, old-swapchain replacement, legacy/synchronization2 barriers, render-pass2, and imageless framebuffers. This test installs dispatch directly; the separate application runs below exercise loader bootstrap.
- All five existing renderer GPU regressions passed after the Vulkan changes.
- Built the Vulkan hook and debug viewer successfully; existing unrelated warnings remain.
- Loaded-layer Dolphin Vulkan runs on the RTX 5070 Ti delivered over 3,720 frames to D3D12 and over 12,000 frames to Vulkan at about 60 FPS, using the configured 640x528 three-slot stream. Both showed the animated Melee intro and exited normally. These are integration examples; the implementation and GPU tests are independent of Dolphin. Other vendors, controller gameplay, and isolated desktops were not validated.

Reproduce with the earlier viewer command using `--hook vulkan` and Dolphin `-v Vulkan`; choose either `--api d3d12` or `--api vulkan`. Logs and screenshots are under `target/vulkan-wsi-validation/`. Final rebuilt-hook smoke runs also passed with both consumers, advancing hundreds of frames and exiting with code zero; their logs contain no warnings/errors.

## Broader remaining work

OpenGL default-framebuffer virtualization remains unfinished. Broader Vulkan WSI profiles, production DDI coverage, concurrent deferred recording, other vendors/drivers, and additional guest lifecycle scenarios remain to be demonstrated. See VIRTUAL_SWAPCHAIN_HANDOFF.md.

