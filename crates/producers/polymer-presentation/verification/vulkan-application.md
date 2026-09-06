# Vulkan application comparison

Verified on 2026-09-06 using SDK 1.4.350.0 `vkcube.exe`, the native Vulkan
loader, and the D3D12-importing viewer fixture in a separate process.

The baseline is detached commit `c20e76d7878e9c095e3bc25cfa3f076bb49070b0`,
built without tracked-file changes at `target/presentation-baseline`.
The current layer was rebuilt from this uncommitted refactor. Both used the
same installed driver and external `xa11y` dependency checkout.

| Layer | Tracking | Result |
| --- | --- | --- |
| Baseline | Disabled | Imported frame 1, 320x240, 1,222 distinct colors |
| Current | Disabled | Identical dimensions and pixel bytes to baseline |
| Baseline | Graph | Accepted 320x240 stream; no frame published during 120 presents |
| Current | Graph | Imported frame 1, 240x240, 1,222 distinct colors |

The current graph output equals `baseline.crop((40, 0, 280, 240))` byte-for-byte.
This preserves the rendered viewport and removes only the surrounding allocation
area. The baseline graph case fails the frame-publication assertion after the
application exits. Its layer log confirms acceptance and 120 presents. This is
a recorded behavior difference, not a passing baseline graph test. The current
implementation removes the logical-ID-based backbuffer rejection that could
leave graph fallback streams without frames.

SHA-256 of the PPM artifacts:

| Artifact under `target/` | SHA-256 |
| --- | --- |
| `vkcube-baseline-whole.ppm` | `a6dcd57c996b941096e8bb63b423428c8688ad970fe67197eb0b2d5522ee248e` |
| `vkcube-current-whole.ppm` | `a6dcd57c996b941096e8bb63b423428c8688ad970fe67197eb0b2d5522ee248e` |
| `vkcube-current-graph.ppm` | `f11b29e4facaca9101193e91fa1c56c81f96cecf7b966416317b4d59d46411a4` |

Corresponding PNGs are available for inspection. Test logs are named
`vkcube-baseline-whole-test.log`, `vkcube-baseline-graph-test.log`, and
`vkcube-current-{whole,graph}-test.log`. The baseline graph layer log is
`vkcube-baseline-graph-layer.log`.

## Reproduction details

Run the ignored `external_vulkan_application_publishes_importable_pixels` test
from the current checkout with a private `POLYMER_SESSION` and:

- `POLYMER_APPLICATION_PATH`: absolute path to SDK `vkcube.exe`.
- `POLYMER_APPLICATION_LAYER_PATH`: absolute expected layer DLL path. The test
  verifies the module loaded in the application process before driving frames.
- `POLYMER_APPLICATION_CAPTURE`: output PPM path.
- `VK_LAYER_PATH` and `VK_IMPLICIT_LAYER_PATH`: test manifest directory.
- `VK_INSTANCE_LAYERS=VK_LAYER_POLYMER_capture`, `POLYMER_VULKAN_LAYER=1`.

For graph mode set `POLYMER_TRACK_RENDER_TARGET=1`,
`POLYMER_TRACK_RENDER_TARGETS=0`, and `POLYMER_GRAPH_TRACKING=1`.
For disabled tracking **remove** those variables; setting them to `0` still
enables the presence-based flags. No CLI configuration semantics were changed.

Use a manifest with an **absolute** `library_path` for the isolated baseline.
In this environment the checked-in relative path resolved to the current DLL
despite discovery of the baseline manifest. Those early attempts were rejected
by the loaded-module assertion and are excluded from the comparison. The stale
unverified graph artifact was removed. The successful baseline manifest is at
`target/presentation-baseline-manifest/polymer-layer.json`.
Optional `POLYMER_APPLICATION_LOADER_LOG` redirects application stderr for
`VK_LOADER_DEBUG` diagnostics. Explicit and implicit discovery are separate,
as documented by the [Vulkan loader](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderApplicationInterface.md#forcing-layer-source-folders).

This verifies one representative Vulkan application. OpenGL application
comparison and broader application coverage remain separate validation work.
