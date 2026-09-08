# polymer-presentation

Shared presentation lifecycle and image publication for Polymer graphics producers.

D3D11, D3D12, and Vulkan render directly into shared swapchain images at the guest-requested creation extent. OpenGL renders into WGL pbuffers sized from the initial client area and exports them with one GPU blit into an EXT external-memory image ring. GDI paints into shared DXGI surfaces.

The crate owns fixed-surface matching, swapchain controllers, buffer acknowledgments, pacing, retirement, frame placement, and IPC publication. Native allocation and synchronization remain in each backend. The small presentation registry retains logical surface identifiers and window selection; title-qualified requests claim matching windows before positional requests.

There is no render-target observation, dependency graph, graph selection, resource history, capture preparation, or render-command interception. Internal emulator screen textures are not selected or split out of a window.

The default feature set contains computational presentation policy. The `producer` feature adds Windows IPC publication and lifecycle integration.

Verification:

```powershell
cargo test -p polymer-presentation --lib
cargo test -p polymer-presentation --features producer --lib
```
