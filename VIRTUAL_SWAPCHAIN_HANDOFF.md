# Fixed-resolution virtual swapchains: refined design

The active design adds explicit buffer-availability acknowledgments. It supersedes the earlier prohibition against IPC and consumer changes. Do not reintroduce software command replay, logical D3D11 texture/view wrappers, or native recordings duplicated per ring slot.

D3D11, D3D12, and Vulkan use native shared swapchains. OpenGL uses fixed WGL pbuffer swapchains with one GPU blit through EXT external memory; framebuffer zero remains driver-managed. Optional Vulkan presentation features remain. See VIRTUAL_SWAPCHAIN_IMPLEMENTATION.md for current status.

## 1. Configuration and architectural ownership

- D3D11, D3D12, and Vulkan support only renderMode: "swapchain" (the default). Their interception capture and graph tracking are removed; do not restore compatibility paths. OpenGL also requires "swapchain"; only GDI uses "intercept". Graph tracking, NV/CPU capture fallbacks, and keyed-mutex transports are deleted.
- Every swapchain surface requires an immutable positive resolution: [width, height]. Preserve configured index, public pattern, rotation, and display order.
- Match actual HWND lifetimes and titles. Image indices and swapchain creation order are not configured surface identities.
- One active graphics producer owns the assignments for a launch. Support destruction/recreation at the same configured extent, device loss, disconnect, and retirement.
- Reject virtual resize requests. Never replace resources within an accepted generation.
- polymer-presentation owns configuration, matching, controller state, publication, acknowledgments, pacing, and retirement policy.
- Producer crates own graphics resources, native synchronization, and API-specific presentation behavior.
- Hook crates own interception. D3D11 DDI discovery, adapter/device callback interception, resource-handle capture, and callback lifetime management belong in polymer-hook-d3d11.
- The D3D11 producer receives a rotation interface. It must not load UMDs, patch DDI tables, or inspect private runtime COM layouts.
- Keep ARCHITECTURE.md unchanged; it is human-maintained.

The presentation configuration schema lives in polymer-protocol to avoid a dependency cycle through the hook crate. polymer-presentation reexports its configuration module.

## 2. IPC availability and native GPU ownership

Protocol version 18 adds RequestBufferAvailability before StreamReady for virtual streams, and BufferAvailable { stream_id, through_sync_value } from consumer to producer.

The acknowledgment is cumulative through the specified publication value. The connection identifies the immutable resource generation. Ordinary capture streams do not subscribe and do not accumulate unread responses.

Send the acknowledgment only after submitting the native consumer release signal. The message authorizes reuse; it does not prove GPU completion. The producer additionally checks the shared release fence and establishes the API acquire dependency.

- A retained image releases only older publications, through current.sync_value - 1.
- A cached image releases through current.sync_value after its copy/read is ordered.
- Skipped frames are covered by the cumulative watermark.
- Failed/unsubmitted consumer recording does not acknowledge release.
- Bind the release endpoint to the imported connection, never a mutable HWND/current surface.
- Ignore duplicate/older watermarks without regression. Reject foreign streams and future publications.
- Leave partial control packets unconsumed until a complete message is available.
- Never carry acknowledgments into a replacement connection.

Never-published buffers are available after StreamAccepted. Recycled slots require producer completion, IPC availability covering their previous publication, and native external reacquisition.

The existing frame mapping, FrameReady metadata, NT-handle transfer, and StreamReady/StreamAccepted remain. No new image or synchronization discriminator is needed. Both consumers support the existing fence-synchronized D3D11 NT-handle profile.

## 3. D3D12 native indexed buffers

GetBuffer(index) returns fixed native ID3D12Resource objects. Cached resources and descriptors keep their native identities.

The application follows ordinary DXGI rules: render into the current backbuffer, order contributing queues onto the presentation queue, and return the resource to PRESENT/COMMON before Present. Signal producer readiness on that queue and publish the physical slot.

Initial creation acquires buffer zero. Before successful Present returns, acquire the next indexed buffer using IPC availability and the completed release fence. GetCurrentBackBufferIndex therefore identifies a buffer ready for application rendering.

DO_NOT_WAIT must reserve the next buffer before publication so a retryable busy result cannot publish twice. A failure after publication terminates the generation rather than inviting a duplicate Present retry.

Remove virtual-only command resource census, shader/descriptor access scanning, ExecuteCommandLists routing, resource-owner tombstones, cross-queue bridges, private-submission exemptions, reconstructed resource states, and per-image release command lists/allocators. Remove their obsolete tests and switches. Graph-tracking machinery has been removed for every backend.

Polymer does not repair application queue-ordering or resource-state errors in swapchain mode.

## 4. D3D11 native DDI rotation

Return a real native texture for logical buffer zero. Views, bindings, and deferred command lists stay native. An application records a command list once; Polymer does not replay its commands or compile variants per slot.

The hook installs before application device creation:

1. Discover the installed UMD through Windows adapter information.
2. Intercept OpenAdapter10_2 and returned CreateDevice/CloseAdapter callbacks.
3. Capture actual driver device/resource handles during ring texture creation.
4. Preserve interception across RelocateDeviceFuncs.
5. Remove registrations on device destruction/adapter close; retain callback code and driver lifetime.
6. Return a resource-retaining rotation object through the producer's rotation interface.

The producer maps native texture identities to exported physical allocations. After acquiring the next render allocation, flush the immediate batch, exchange buffer-zero's identity with that allocation through the DDI, and update the permutation. Exported handles and consumer imports remain attached to physical allocations.

The DDI contract requires the driver to update native views and bound state during rotation: [DXGI_DDI_BASE_FUNCTIONS](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dxgiddi/ns-dxgiddi-dxgi_ddi_base_functions).

Use shared fences with both consumers. Keyed-mutex alternatives and OpenGL NV interop have been removed. Serialize raw rotation through ID3D11Multithread protection. Retain native textures for the rotation object's lifetime. Reject missing capture instead of guessing COM offsets or falling back to replay.

Cover factory creation and D3D11CreateDeviceAndSwapChain. Route the combined export through the same virtual factory boundary before exposing buffers.

Dolphin validation now covers the production DDI path with both D3D12 and Vulkan consumers, including RGB10A2 UNORM buffers. See the implementation status for the exact run scope and reproduction command. The debug viewer accepts --swapchain-resolution WIDTH HEIGHT; runtime virtual launches support D3D11, D3D12, and Vulkan hooks.

## 5. Lifecycle and pacing

Publish handles once, wait for consumer import acceptance, and acquire the initial render lease before exposing buffers. Keep fixed extent, logical identity, descriptors, current index, and window matching coherent.

Waitable latency reflects controller pacing and wakes on terminal failures. Disconnect terminates the immutable frontend; it never overwrites cached resources with a replacement generation.

Retirement retains resources until producer work and consumer release are safe, and quarantines uncertain work. D3D11/D3D12 use shared DXGI maintenance; OpenGL retirement stays on a valid context owner.

Native query/counter side effects execute exactly once per application execution. No replay modes, duplicated recordings, or acknowledgment-bypassing fallbacks are permitted.

## 6. Vulkan: WSI interception and external ownership

The fixed Win32 profile below is implemented without a command recorder. See VIRTUAL_SWAPCHAIN_IMPLEMENTATION.md for tested behavior and remaining software limits. The following requirements remain the design reference for broader profile work.

### Dispatch and surface capabilities

Implement routing through both `vkGetInstanceProcAddr` and `vkGetDeviceProcAddr`, including relevant core/KHR aliases and statically exported entry points. Keep downstream dispatch pointers for internal calls so the layer does not intercept its own barriers or submits. Register internal command buffers with loader dispatch data using the existing layer mechanism.

Retain the real Win32 surface for HWND association and downstream native use, while virtual swapchains are separately tagged non-native objects. Validate every handle lookup by device/generation. Never pass a virtual handle downstream. Mixed native and virtual objects must coexist on one device.

Intercept and keep coherent:

- Surface support, surface capabilities and capabilities2.
- Surface format and formats2 enumeration, count-only calls, short arrays, and `VK_INCOMPLETE` behavior.
- Present-mode enumeration and any supported extension-specific capability queries.
- Swapchain creation, image enumeration, acquire1/acquire2, queue present, and swapchain destruction.

For matched surfaces advertise the configured fixed extent and only image counts, formats, usage bits, transforms, alpha modes, present modes, and queue support that the virtual backend can implement. Validate external-memory support for the exact format/tiling/usage combination. Do not publish a native surface capability list and then fail every valid-looking request because the virtual images have different restrictions.

Feature injection at device creation must walk existing `pNext` structures. Do not duplicate timeline feature structs, overwrite an application's explicit values, or enable unsupported features. Handle Vulkan 1.2 feature blocks and extension feature blocks coherently. If a required capability cannot be enabled legally, reject virtual support before constructing synthetic WSI objects. Keep application binary acquire/present semaphores separate from transport timeline semaphores.

### Creation and application acquire

1. Resolve the HWND binding and fixed extent before allocating the virtual image set.
2. Validate requested image count, format/color space, extent, layers, usage, sharing mode/queue families, present mode, and every relevant `pNext` feature. No silently ignored protected, device-group, fullscreen-exclusive, mutable-format, maintenance, present-ID/wait or other extension behavior.
3. Create transport-compatible exportable images and native application views of those images; enumerate the real image handles with correct Vulkan count semantics. Keep internal handles and generation ownership private.
4. Acquire selects any actually available slot under controller policy. Vulkan permits several outstanding acquired images and presentation in an order different from acquisition; there is no universal `next_index = present_count % count` rule.
5. `timeout == 0` returns `VK_NOT_READY` when unavailable; finite expiration returns `VK_TIMEOUT`. Do not modify the output image index or signal an app semaphore/fence on a failed acquire.
6. Successful acquire must arrange the app's requested binary semaphore/fence signaling and image ownership availability with valid Vulkan submissions. Honor both acquire entry points and supported device-mask behavior. Signal each primitive exactly as the Vulkan contract requires.

Choose and document an internal queue policy from queues actually created on the device. Do not assume the first queue is graphics-capable, compatible with all present work, or externally synchronized by the app. Internally serialize private queue use, and coordinate with intercepted app queue operations when sharing an app-owned queue. Account for queue external-synchronization requirements without holding global registry locks.

### Present, dependencies, and partial results

`vkQueuePresentKHR` can contain several native and virtual swapchains with one shared set of wait semaphores. Those binary waits must be consumed **once**. A split implementation that independently waits them in every virtual submission and the native present is invalid.

Build one dependency-consumption/fan-out operation: consume application waits once, produce private bridges for the native present and virtual publication paths, and retain bridges until their consumers complete. Correctly handle no-native, no-virtual, and mixed arrays. Preserve per-swapchain `pResults` and the aggregate result, including failures that still consume waits. Do not early-return on the first bad virtual entry and leave other entries' synchronization in an invented state.

Join each virtual image's rendering and release operations into an ordered ready-signaling sequence. Publish each completed submission with its own slot/value. An app present wait is not the same as a consumer-release wait, and app queue-family transfer is not the same as external consumer ownership transfer.

Follow the actual presentation ordering and wait rules: [vkQueuePresentKHR](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueuePresentKHR.html). Semaphore reuse requires completion evidence, not simply returning from present: [Vulkan swapchain semaphore reuse guide](https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html).

### Layout and queue-family translation

The guest sees WSI images and can use `PRESENT_SRC_KHR`. The exported resource must satisfy the existing transport's external layout and owner. Maintain logical application state separately from physical export state, per image/subresource and queue family as required. Translate only resources belonging to virtual swapchains; normal native images remain untouched.

Coverage includes:

- Legacy image memory barriers and synchronization2 barriers, including core/KHR aliases.
- Event wait dependency paths that contain image barriers.
- Render pass initial/final layouts, render pass2, framebuffer/image-view associations, imageless framebuffers, and secondary command buffers.
- Dynamic rendering and its actual image layouts/attachments; do not assume every transition appears in one barrier hook.
- Supported ownership transfers between app queues, plus the separate external/foreign handoff required by the selected Win32 image profile.

A render-pass object can be used with both virtual and native attachments. Do not globally rewrite its create-info to replace every `PRESENT_SRC_KHR`. Use resource-aware execution bookkeeping and compatible internal variants where required; preserve the app-visible render-pass/framebuffer compatibility rules and lifetimes. Reject unsupported constructions before they can perform invalid driver work rather than silently emitting the wrong layouts.

The prototype's observed `VK_QUEUE_FAMILY_FOREIGN_EXT` behavior is evidence for its tested Win32 transport profile, not a universal constant for every external handle type. Verify against current importer behavior and capability requirements. Synchronization must establish both memory availability/visibility and ownership: [Vulkan synchronization specification](https://docs.vulkan.org/spec/latest/chapters/synchronization.html).

### Destruction and same-size recreation

Honor `oldSwapchain`: retirement prohibits new acquisitions from the old swapchain, while images acquired before retirement can still be presented under Vulkan's rules. A replacement can use the same logical surface ID and same configured extent but has a distinct generation, image set, and synchronization lifetime.

Do not free an old image because a new `StreamReady` was accepted. Wait for app references/valid API destruction rules, producer work, and consumer ownership release. Device loss and failed presents need the typed indeterminate/retirement paths. Native window resize alone does not change the fixed virtual extent or force a reallocation.

The prototype demonstrated feasibility for a narrow path. Full completion requires the hook and extension inventory above, realistic queue use, and validation of render-pass layouts; merely moving its fake handle map into presentation is insufficient.

## 7. OpenGL: fixed WGL pbuffer swapchains

OpenGL requires swapchain mode and configured extents. Route WGL draw/read bindings and swaps to independent `WGL_ARB_pbuffer` drawables. Preserve application-facing DC identity, native framebuffer-zero behavior, and context-affine cleanup. Report the bound fixed extent through `GetClientRect`; physical HWND resize must not recreate presentation resources.

Reuse the EXT external-memory/semaphore ring with one blit per published frame. There is no HWND capture path, NV intermediate, keyed mutex, CPU readback, or application rendering-state tracker. Pixel-format compatibility and exact pbuffer allocation are mandatory. Preserve separate read/draw DCs around publication. Details and the native WGL regression command are in `crates/producers/polymer-producer-opengl/INTEROP.md`.

## 8. Validation and remaining work

The user removed the standalone swapchain probe. Do not recreate it or retain commands referencing it.

Use the remaining IPC, presentation, producer, runtime, and renderer tests, including native consumer GPU regressions where available. Exercise acknowledgment fragmentation, stale connections, skipped/retained publications, and failure ordering.

Required production DDI evidence includes cached RTV/SRV use, a native command list recorded once and executed across rotations, native side effects, concurrent deferred recording, both consumers, and same-size recreation/disconnect. Earlier isolated feasibility experiments do not establish that the new production integration passes.

Validate D3D12 application barriers and queue ordering, native cached buffers, blocking/nonblocking Present, and waitable pacing with real acknowledgments. Do not preserve tests that expect Polymer to infer arbitrary application accesses.

The broader matrix remains all four producers, their consumer profiles, Vulkan WSI/queue-family semantics, and OpenGL pbuffer presentation, resize independence, and context cleanup. Unimplemented software is remaining work, not an unsupported hardware profile.

