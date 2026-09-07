> Historical feasibility notes. The standalone probe and alternative replay/recording paths have been removed. VIRTUAL_SWAPCHAIN_HANDOFF.md and VIRTUAL_SWAPCHAIN_IMPLEMENTATION.md describe the active IPC acknowledgment and hook-owned DDI design. Results below do not validate the new production integration.

# Minimal D3D11 buffer rotation investigation

Command replay is not a requirement of buffer rotation. It is one implementation choice when emulating resource identity above the native runtime. The earlier implementation expanded into a software context before establishing the narrow rotation contract.

## Required behavior

1. Preserve the application-visible backbuffer and view identities while selecting a consumer-released shared allocation for the next frame.
2. Resolve immediate-context resource/view arguments that reference that backbuffer. Ordinary textures, buffers, shaders, draws and Map/Unmap do not intrinsically need recording.
3. Apply the advertised swap effect's state semantics. Flip-model Present unbinds writable backbuffer bindings (except applicable DO_NOT_SEQUENCE behavior); preserving a sticky OM target across normal Present is not the native requirement. Keep identity getters and hazard-visible state consistent.
4. Previously recorded command lists that reference a backbuffer must observe its rotated identity at execution. Binding a new RTV on the immediate context cannot repair a command list's explicit old resource references.
5. Establish next-image write ownership before application work, and publish current-image readiness after its GPU writes. An IPC availability acknowledgment can express the former but cannot itself alter resource references.

## Native mechanism and API boundary

Microsoft documents pfnRotateResourceIdentities in DXGI_DDI_BASE_FUNCTIONS. The runtime calls the user-mode display driver to rotate allocation/kernel identities while retaining runtime handles. The driver must update affected views and, where needed, bindings. This is a runtime/driver DDI with driver handles, not a public ID3D11DeviceContext method accepting application COM textures. Its existence means full application command replay is not intrinsically necessary. Whether this hook architecture can safely reuse it for ordinary shared textures is unproven; invoking it out of band cannot be assumed safe for runtime state, resource flags, driver command lists or imported consumer allocation identities.

The D3D11 functional specification section 6.3.9.2 explicitly requires rotation to affect command lists containing backbuffer references. Section 6.3.3 disallows immediate context state inheritance by command lists. This is the specific deferred-context requirement; it does not prescribe a software interpreter for all commands.

## Executed experiment

`cargo run -p polymer-swapchain-probe -- --d3d11 --rotation-minimal`

Unhooked D3D11, two ordinary shared NT-handle textures A/B, a native deferred list binding and clearing A, then immediate-context binding B before executing that list. GPU staging readback verifies A becomes red and B stays black, for both ExecuteCommandList restore modes. This rules out merely rebinding B before execution; it does not test driver-level identity rotation, nor prove replay is the only possible alternative.

Local Dolphin D3D source search found no CreateDeferredContext, FinishCommandList or ExecuteCommandList calls in Source/Core/VideoBackends/D3D. Its StateManager::Apply binds framebuffers through OMSetRenderTargets or OMSetRenderTargetsAndUnorderedAccessViews. This makes immediate-context view translation a relevant first emulator experiment, not proof that all supported applications avoid deferred rendering.

## Consequence for scope

For immediate-context rendering, use a logical backbuffer/view table and translate the operations that actually consume those aliases; forward unrelated application work natively. Do not capture ordinary buffer maps or reproduce all pipeline state merely to rotate an immediate backbuffer. For complete deferred support, choose and verify a native identity mechanism or an explicitly bounded translation strategy; keep that decision separate from basic rotation. No replay removal or replacement is claimed implemented by this investigation.

## Sources

- https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dxgiddi/ns-dxgiddi-dxgi_ddi_base_functions
- https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm (6.3.3, 6.3.9.2)
- https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/d3d10-graphics-programming-guide-dxgi
- https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-present

## Deferred alternatives tested without an API replay interpreter

### Native allocation remapping through tiled resources

A new `--tiled-rotation` probe creates one native tiled Texture2D/RTV, two permanent alias textures, a two-tile pool, and two native command lists recorded once against the unchanged RTV. It alternates UpdateTileMappings on the immediate context before native ExecuteCommandList, using TiledResourceBarrier between alias accesses. Full 128x128 staging readback verifies the selected tile's contents and the retained other tile over 12 rotations. This passes on the local RTX 5070 Ti. No app API command stream is saved or interpreted by the probe.

The first default D3D11 device reported tiled tier 1. Explicitly requesting feature level 11.1 reports tier 2; the probe now does so for this experiment. Private pool rotation passes in both tested tiers. This establishes a native deferred allocation-remapping mechanism, not consumer sharing.

Sharing is not working in this test. CreateBuffer rejects TILE_POOL combined with SHARED, SHARED|NTHANDLE, or KEYEDMUTEX|NTHANDLE. At tier 2 the debug layer explicitly reports that tile pools cannot be created as shared and NTHANDLE is only available for 2D surfaces. A D3D12-created shared heap also fails D3D11 OpenSharedResource1<ID3D11Buffer> with E_INVALIDARG, including a separately created 11-on-12 device. Do not claim this is an end-to-end shared-image solution or assume an IPC-only change suffices. Even with sharing support, consumers would need compatible tiled resource/heap import and stable per-slot mappings rather than their current ordinary-texture importer.

Commands:

```
cargo run -p polymer-swapchain-probe -- --d3d11 --tiled-rotation
cargo run -p polymer-swapchain-probe -- --d3d11 --tiled-rotation --tile-shared --debug
cargo run -p polymer-swapchain-probe -- --d3d11 --tiled-rotation --tile-heap
cargo run -p polymer-swapchain-probe -- --d3d11 --tiled-rotation --tile-on12 --tile-heap
```

Only the first passes; the remaining commands retain reproducible sharing failures. Current --tile-shared selects KEYEDMUTEX|NTHANDLE. Earlier SHARED-only and SHARED|NTHANDLE attempts also failed. The public tiled-resource specification describes cross-device tile pools, but this is not proof that the tested creation/import combinations work on the installed runtime.

### Native command-list variants per slot

`cargo run -p polymer-swapchain-probe -- --d3d11 --rotation-minimal --native-variants`

The probe records native lists for each of two ordinary shared textures, and selects the appropriate native list at execution. GPU readback checks that only the selected allocation was cleared, for both slots and both restore modes. Four cases pass. This is a narrow proof of selection among native recordings, not a finished deferred-context proxy.

A full implementation could fan out each deferred recording call into one native context per backbuffer slot, substituting only backbuffer aliases. FinishCommandList returns a wrapper containing native variants; ExecuteCommandList selects one and forwards it. Native D3D11 handles pipeline state, queries, buffer renaming and execution. No serialized command interpreter is needed at execution. However, this still requires forwarding the deferred API surface, mirroring mapped writes before Unmap, native nested variant selection, and potentially a Cartesian product for command lists referencing independently rotating swapchains. It reduces interpreter/state emulation, but does not prove the desired small hook footprint. Do not present it as free or as avoiding duplicated recording.

### Remaining decision

If requiring one native recording, ordinary shared textures, direct rendering and full deferred semantics together, runtime/driver identity rotation remains the mechanism to investigate. The DDI is documented; safely capturing driver resource handles, ordering runtime/driver work and preserving native command-list patching without private-runtime assumptions has not been implemented or tested. Do not resume the broad software replay implementation merely because these alternatives require further work.

Additional sources:

- https://learn.microsoft.com/en-us/windows/win32/direct3d11/tiled-resource-apis
- https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html
- https://github.com/microsoft/D3D12TranslationLayer/blob/master/src/SharedResourceHelpers.cpp

## Hooked RotateResourceIdentities: successful native-driver probe

`cargo run -p polymer-swapchain-probe -- --d3d11 --ddi-rotation --debug`

Tested on NVIDIA GeForce RTX 5070 Ti, Windows driver 32.0.16.1062. This probe intercepts nvwgf2umx.dll!OpenAdapter10_2 with the repository's InlineDetour, wraps the returned adapter CreateDevice callback, captures the returned DXGI RotateResourceIdentities function, and wraps the device CreateResource/RelocateDeviceFuncs callbacks. A thread-local creation tag associates the two test textures with actual driver device/resource handles. No private COM-object offsets are inspected. DDI struct prefixes and callback-table positions come from Microsoft's d3d10umddi.h and the installed dxgiddi.h. Observed adapter interface 0xb0011, device interface 0xb002d/version 0x10000; DriverCommandLists=TRUE.

Both textures are ordinary native DEFAULT RGBA8 textures with RTV/SRV bindings and SHARED|SHARED_NTHANDLE. No BIND_PRESENT modification is made. The probe creates cached RTVs and native draw/clear command lists before invoking rotation. A second D3D11 device imports both original shared handles before rotation.

Verified by GPU readback:

- RotateResourceIdentities returns S_OK and exchanges the allocations behind the producer resources.
- The separately imported textures retain their original physical allocation identities.
- A cached RTV used for immediate clear follows rotation.
- Pre-recorded native DrawInstanced and clear command lists follow rotation in both directions, without recording variants or interpreting application commands.
- The draw fixture's occlusion query returns 4096 samples; every pixel of each 64x64 output is checked.
- 120 further rotations reuse the same native draw/clear lists, alternate both ExecuteCommandList restore modes and verify that the non-current imported allocation is preserved.
- No D3D11 debug-layer ERROR or CORRUPTION messages are reported.

This supersedes the earlier statement that hooked driver identity rotation had not been tested. It is concrete evidence of a much smaller non-replay mechanism on this driver. Production integration is not implemented: the probe is NVIDIA-specific, uses serialized GPU readback to establish ownership/completion, does not run an injected emulator or IPC consumer, and does not establish other driver/version coverage or concurrent deferred recording safety. It uses a second D3D11 device as the independent shared-allocation reader, not the full D3D12/Vulkan renderer. Hook lifetime is process-scoped in the experimental binary.

Probe sources: bins/polymer-swapchain-probe/src/d3d11/ddi_rotation/{ffi,hooks,mod}.rs. The production software replay implementation has not been removed by this experiment.

Header source: https://raw.githubusercontent.com/microsoft/wdkmetadata/main/generation/WDK/RecompiledIdlHeaders/um/d3d10umddi.h (downloaded into ignored target/ddi-rotation for inspection).
