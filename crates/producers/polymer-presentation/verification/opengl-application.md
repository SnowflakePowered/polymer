# OpenGL application comparison

Verified on 2026-09-06 using installed FFplay 8.1.2, its SDL OpenGL renderer,
and a D3D12-importing viewer in a separate process. FFplay runs on a private
desktop through Polymer's existing injector and kill-on-close process job.
The workload is `smptebars=size=320x240:rate=30:duration=4`.

The baseline is detached commit `c20e76d7878e9c095e3bc25cfa3f076bb49070b0`,
built without tracked-file changes at `target/presentation-baseline`.
The current hook was rebuilt from this uncommitted refactor. Both use the
same installed driver, application, and OpenGL EXT external-object transport.
The injector receives an absolute DLL path; each test log records that path.

| Hook | Tracking | Imported result |
| --- | --- | --- |
| Baseline | Disabled | Frame 1, 320x240, 99 colors, bottom-up storage |
| Current | Disabled | Identical pixels, dimensions, and orientation |
| Baseline | Graph | Frame 1, 320x240, 14 grayscale values, top-down storage |
| Current | Graph | Identical to the whole-frame color output |

The baseline graph log selects uploaded texture 1. Its grayscale output is
consistent with selecting the video's luma plane before the fragment shader
combines the video planes. The shared selector requires rendered-content evidence
for uncertain sampled candidates, so this workload falls back to the observed
backbuffer instead. This is an intentional selection change; all four runs
successfully publish and import a frame, but the baseline graph output does not
reproduce the application's final colors. All runs advertise the same full-window
axis-aligned presentation mapping.

SHA-256 of the raw-storage PPM artifacts under `target/`:

| Artifact | SHA-256 |
| --- | --- |
| `ffplay-baseline-whole.ppm` | `9b144471613d61c927d6adaa2aa17e50d2ba826e777c68ee706bfa86b0d95be3` |
| `ffplay-current-whole.ppm` | `9b144471613d61c927d6adaa2aa17e50d2ba826e777c68ee706bfa86b0d95be3` |
| `ffplay-current-graph.ppm` | `9b144471613d61c927d6adaa2aa17e50d2ba826e777c68ee706bfa86b0d95be3` |
| `ffplay-baseline-graph.ppm` | `5849e687b7e34e0e8c68c87c9318a879ce6e5b3b0bafdfd5c48f85707313568a` |

The PPM files preserve shared-image row order. Apply the logged `bottom_up`
flag when viewing them upright; this does not change the comparison above.
Logs are `target/ffplay-{baseline,current}-{whole,graph}-test.log`.

## Reproduction

Build `polymer-hook-opengl32` and `polymer-injector`. Run the ignored
`external_opengl_application_publishes_importable_pixels` test from the current
checkout with a unique private `POLYMER_SESSION` and these variables:

- `POLYMER_APPLICATION_PATH`: absolute installed `ffplay.exe` path.
- `POLYMER_APPLICATION_LAYER_PATH`: absolute baseline or current hook DLL path.
- `POLYMER_APPLICATION_ARTIFACTS`: directory containing `polymer-injector.exe`.
- `POLYMER_APPLICATION_CAPTURE`: optional output PPM path.

For graph mode set `POLYMER_TRACK_RENDER_TARGET=1`,
`POLYMER_TRACK_RENDER_TARGETS=0`, and `POLYMER_GRAPH_TRACKING=1`.
For disabled tracking remove those variables; setting presence-based flags to
`0` still enables them. The fixture sets `SDL_RENDER_DRIVER=opengl` in the child
environment and waits at most 15 seconds for a nonuniform captured frame.

The application comparison currently requires EXT external-object transport.
Separate native fixtures exercise EXT, NV interop, and PBO capture with texture
and renderbuffer sources. This application comparison does not cover every game,
driver, or context-destruction failure mode.
