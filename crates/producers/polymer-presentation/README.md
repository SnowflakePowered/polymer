# polymer-presentation

Shared presentation-source selection for OpenGL, Vulkan, D3D11, and D3D12.
The selector identifies emulator screen textures before swapchain composition,
including multiple screens and cropped content within padded allocations.

## Data flow

1. API adapters record rendered targets and sampled/copy/blit/resolve dependencies.
2. Deferred adapters publish observations when command lists execute.
3. `ObservationStore::present` advances and expires evidence, returning grouped dependencies.
4. The selector produces `SurfaceSelection`, carried intact through history to frame publication.
5. History reconciles page flips with stable logical screen slots.
6. The registry assigns requested slots and windows. Each backend validates its native source and performs capture and synchronization.

`Surface` supplies fallible extent metadata. `PresentationGraph` exposes a
selection-scoped view of resources and dependencies. Native adapters retain
resource ownership and execute metadata queries on the appropriate context.
The selector caches metadata during selection and makes no graphics API calls.

`ObservationStore` deduplicates observations and expires old generations. Ordinary
tracking considers immediate dependencies; graph tracking traverses intermediate
surfaces. Exact mapping evidence takes precedence over sampled guesses. Source
crop and destination placement remain separate, including reflection, because
placement also supports input-coordinate mapping.

`ResourceHistory` uses `HistorySurface` identity and compatible layout metadata to
keep screen slots stable across page flips and briefly retain known placement. Providers revalidate native resources
before capture. Window request matching preserves configured indices and titles.

## Dependencies

Default features are computational and do not depend on graphics, hooks, Windows,
or IPC. The optional `producer` feature supplies shared IPC session and export
publication support and a small connection retry helper. Graphics allocation and
synchronization remain backend-owned. Vulkan window-routing compatibility stays
in its producer.

## Verification

The CPU-only `CpuBackend` submits metadata through the real store, selector, history,
registry, and frame construction. It needs no graphics runtime, windows, or IPC.
Shared fixtures cover padded allocations, unequal dual screens, intermediate
mappings, ambiguous sampled inputs, reflections, page flips, subresources, cycles,
and selection limits. Backend recording tests cover submission and reset behavior.

```powershell
cargo test -p polymer-presentation --no-default-features --lib
cargo test -p polymer-presentation --features producer --lib
```
