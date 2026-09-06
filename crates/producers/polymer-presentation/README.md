# polymer-presentation

The default feature set is computational: it has no hooks, graphics API,
window-system, or IPC dependencies. The optional `producer` feature enables
Windows session and publication integration.

## Computational contracts

- `Surface` exposes fallible metadata through an object-safe interface.
  `PresentationGraph::surface` lends that interface to the selector; native
  adapters own the resources and guards. Each selection caches metadata reads.
- `ObservationStore` deduplicates and retains generation-stamped dependencies.
  The selector handles exact evidence, uncertain cohorts, resource page flips,
  subresources, strongly connected components, and bounded mapping traversal.
- `Extent2D` describes allocation dimensions. `Rect` describes a signed-origin
  region. Source crops, destination coverage, and reflected presentation
  mappings remain separate.
- `SelectionHistory` and `ResourceHistory` reconcile layouts and live native
  records. `PresentationRegistry` supplies title precedence, placement pairing,
  geometry assignment, ownership, and presentation-clock expiry.
- `SlotScheduler` advances only after submission. `SlotReservation` and native
  leases roll back ownership when acquisition or submission fails.

## Integrated behavior

OpenGL, Vulkan, D3D11, and D3D12 invoke the shared selector. Vulkan's previous
graph stores, traversal, and submission-time candidate capture path were removed.
OpenGL's dependency observations now use the common store; texture metadata is
scoped per context. Both adapters carry mip/layer identity and source crops into
native copies. Vulkan image incarnations and OpenGL texture deletion invalidate
held resources. OpenGL's concurrent metadata, pending draws, presentation, quad,
and resource-incarnation maps use `DashMap`; deletion removes queued quad placements
as well as dependencies without invalidating sibling resources or other contexts.

D3D11 records deferred batches and publishes them only on execution. D3D12
records command-list batches, resolves SRV tables at submission, tracks legacy
states per subresource, copies RTV descriptors, and expands inherited bundle
draws. Bundle snapshots preserve explicit unbinding and signature invalidation,
and propagate final bindings, viewport, and scissor state to subsequent parent
draws. D3D11 resize releases references to only the affected backbuffers, retaining
unrelated deferred bindings and command batches. Native state and capture-eligibility
checks remain in the producers.
D3D11 capture borrows its owning swapchain, avoiding a COM cycle through the
swapchain hook's capture state. D3D11 resource release relies on the runtime's
[deferred object destruction](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-flush);
it does not clear the application's immediate-context bindings during retirement.
The D3D12 adapter no longer caches presentation rectangles or overrides selected
sources after selection. Raster-only observations use shared clipped fallback
geometry and expire with the graph; a later scissored draw cannot replace proven
copy sources with backbuffer content.
D3D12 ordinary tracking consumes the selected presentation's observations and
resource view; graph tracking retains them. Replaying a command list refreshes
consumed observations without recording the list again.
The shared observation store supplies a transaction guard used by D3D11,
D3D12, and Vulkan for submitted observation batches and presentation
snapshot/clear. Both D3D adapters also use it during resize invalidation.
A concurrent submission cannot lose
its graph edges or resource metadata to the preceding presentation's clear.
A threaded regression verifies that a waiting batch enters the next generation;
the native multi-stream and resize fixtures also pass with this boundary.
Observation deduplication keeps the newest generation/order when a late batch
arrives. Target freshness cannot move backwards, and a new generation replaces
the preceding draw-coverage score. CPU coverage verifies late duplicates, real
replay refresh, reduced coverage, and eventual expiry together.
Both D3D providers publish independent streams for selected logical surfaces.
Resource history preserves vacant logical slots; registry assignments retain
surviving streams through resource removal, page flips, resize, and window reorder.
New resources match surviving identities before compatible slots. Vulkan now
uses the same resource history, retains native streams by logical surface ID,
and matches copies by logical slot instead of dimensions. Vulkan window routing
now uses the computational registry: native adapters supply observations and
window titles, while common code assigns geometry, honors title precedence,
retains identity/slot routes through page flips and resize, and expires gaps.
The old dimension-keyed target cache and local assignment algorithm were removed.
Window/device destruction invalidates routes immediately. OpenGL also records
placement history in the registry using validated logical source slots. That
history preserves page flips, removes incompatible or missing slots, and expires
stale evidence; resize clears the previous window geometry. Its copy engine is
created inside the shared initialization attempt before the session becomes active.
D3D11, D3D12, and OpenGL use optional producer-side `ProducerWindows` to validate
observed process windows before assignment. The computational registry receives
destruction metadata and releases assignments; native resources still retire
under backend synchronization and context-affinity rules. OpenGL validates after
importing stored observations, preventing entries awaiting another context's
cleanup from reintroducing destroyed windows.

All five producers use `ProducerSession`, export-handle preparation,
`CapturedFrame`, and `CaptureSession` states. The wire protocol and existing CLI
configuration are unchanged. Consumer negotiation precedes transport allocation;
acceptance precedes activation. Tagged session failures distinguish protocol
incompatibility and rejection from OS/transport errors, including handle-duplication
permission failures. Transient publication failures retry instead of parking a
stream; OpenGL partial setup is owned before publication so failures clean up.
GDI observes successful `DestroyWindow` calls and disconnects that window's
viewer. Idle and parked sessions are invalidated immediately; outstanding paint
DCs remain owned until EndPaint or successful release recovery. The native paint
fixture exercises this notification during an active paint, verifies the DC
remains usable and no later frame is published, and checks that handle reuse
does not inherit a destroyed window's parked rejection. Persistent ReleaseDC
failure and owning-thread loss still require final-retirement handling.
GDI uses the same explicit setup outcome instead of treating native access-denied
HRESULTs as stream rejection. Unsupported consumers and protocol mismatch park
before surface allocation; native allocation failures remain retryable.
All transports use one `FramePublisher` for send-failure cleanup, with
per-session publication counters and rate-limited diagnostics. A failed stream
releases only its own connection. Publication counters are independent of GPU
synchronization values. OpenGL PBO publication retains the original capture
timestamp. D3D11 and every OpenGL shared-output transport use native leases.
All five providers also poll accepted-session termination before capture using
nonblocking `WSAPoll`, so a busy ring discovers an absent consumer without a
new frame notification. GDI retains an outstanding DC until its owning-thread
paint boundary. Vulkan chains already-submitted presentation waits even when
post-submission connection polling retires the stream.
The NV lease owns both WGL object access and keyed-mutex ownership, returning
unpublished ownership to the producer on an abandoned attempt. EXT timeline
slots commit their release values only after signaling the captured image.
NV interop registers private D3D11 textures and copies them into the shared
NT-handle output after WGL unlock. Direct registration of the exported keyed
texture failed on the validation driver (`0xC007006E`); the private-image path
passed pixel import. This adds one GPU copy per frame. Partially registered
objects and opened WGL device handles are owned during setup, so failure cleans
them up before another DXGI adapter is tried.
EXT timelines own their imported GL semaphore and exported fence handle, so
failure before the capture object exists still releases both. Framebuffer names
are allocated only after image and timeline allocation succeeds.
The specialized PBO readback stages retain their own pipeline scheduling.
Vulkan output slots also use native leases with the shared scheduler. Pending
submission state is separate from fence signaling: recording/OOM failures do not
leave retirement waiting on an unsignaled fence with no submitted work. Device
loss retains pending-resource bookkeeping. D3D12 also uses native slot leases:
submitted work is tracked before fence signaling, independently of publication.
Each slot retains its copied source through completion. Retirement retries failed
signals and defers unconfirmed resources through the shared `RetirementQueue`.
D3D11 now also separates submitted slot values from published release values,
retains fence-backed copy sources through producer completion, and retries a
failed signal before accepting another copy. Failed submissions consume GPU
synchronization values without advancing publication frame numbers.

## Intentional selection changes

- Exact crops can select small or unequal screens without sampled-candidate
  filtering; padding remains outside the published content region.
- A scissor establishes destination coverage, not sampled texture coordinates.
  D3D11 sampled draws without recovered UV mapping publish unknown geometry.
- Only whole-source composites force decomposition once capacity is reached.
  Cropped composites can still decompose when capacity remains.
- Exact composite roots do not make sampled dependencies exact screens. The
  selector carries evidence through complete paths and applies cohort filtering
  to uncertain leaves while preserving proven unequal screens. Exact transfer
  inputs can be selected without an observed render-target write.
- Ambiguous or incomplete mappings do not establish an exact historical layout.
  Exhausting the path budget invalidates partial geometry even when a valid
  mapping was found first. Each selection normalizes current observations once;
  stale exact roots cannot override newer sampled observations.
  Backbuffer fallback runs after final candidate validation, so rejected
  uncertain sources still yield observed content bounds or the full backbuffer.
  Vulkan raster coverage now lives in the common observation store. Shared
  fallback policy uses the newest generation's clipped coverage; a previous
  large viewport no longer pins smaller layouts indefinitely, and stale bounds
  expire with graph observations. The provider's permanent maximum-area cache
  was removed.
- OpenGL transports share coordinate normalization and retain negative
  presentation origins instead of saturating placements above the window.
  Native quad bounds are signed throughout graph selection, placement history,
  and publication. Recovered quads include viewport offsets and retain bounds
  beyond the viewport instead of clamping them into a different placement.
- Explicit placement identities take precedence over draw order. OpenGL quad
  observations retain resource incarnation, mip, target, and layer; ordered
  page-flip inference consumes only observations left after identity matching.
- D3D12 no longer attaches unresolved draws to resources merely because they
  later transition to PRESENT. Unknown bindings stay unknown, and the selector
  uses its backbuffer fallback when no target observation exists.
- Vulkan records distinct copy, blit, and resolve evidence, including
  `vkCmdResolveImage`, through the shared transfer-recording adapter. Core and
  KHR `vkCmdCopyImage2`, `vkCmdBlitImage2`, and `vkCmdResolveImage2` use the same
  recorder and blit coordinate normalization. Hook fixtures verify submission,
  reset, crops, and reflected destinations for these newer entry points.
- D3D12 omits same-state transition barriers, which otherwise caused the real
  offscreen-copy scenario to fail when a source was already in COPY_SOURCE.

## CPU-only portability

CPU-only portability is verified with
`cargo check -p polymer-presentation --no-default-features --target x86_64-unknown-linux-gnu --all-targets`.
The matching Linux dependency tree contains only DashMap and its platform-neutral
or Linux dependencies, with no hook, graphics, IPC, or Windows packages. This
cross-target check compiles the library and test targets; it does not execute
the tests on Linux. CPU tests have been executed on Windows.

## Remaining work from the full plan

This is not yet completion of every requested infrastructure extraction:

- All five producers now use `CaptureInitializer` and `CaptureBackend` through
  the shared lifecycle driver for setup retries, rejection parking, connection
  checks, and activation. Initialization executes on the caller's graphics
  context/thread. All five providers also submit through shared
  `CaptureOperation` dispatch, which skips inactive sessions and immediately
  parks a connection lost during publication. OpenGL busy rings return skipped
  captures. GDI keeps a detached scheduler reservation across BeginPaint/EndPaint;
  its native lease owns the DC and commits after release and fence signaling.
  Failed DC release retains the lease for retry on the owning GUI thread;
  no fence is signaled and no replacement paint begins while release fails.
  Both producer completion and consumer release gate subsequent paints. Vulkan
  publishes after dispatch so the engine stays alive while its binary semaphore
  is handed to the present chain. `CaptureSession` now privately owns its state
  and configuration across all five providers. Unchanged requirements retain
  backoff/rejection, changed requirements invalidate only that stream, and page
  flips leave compatible active resources intact. Shared operation counters track
  completion, skips, and failures per logical session, with one rate-limited
  failure diagnostic. Provider-local failure/publication counters were removed;
  native GPU synchronization values remain separate. Retirement still needs further
  audit. OpenGL capture ownership now uses a `DashMap` with per-entry native
  locks. Its idle sweep uses the registry's 600-generation horizon, forgets
  retired ownership, and releases only resources belonging to the current GL
  context. A `wglDeleteContext` prefix now retires capture resources
  before WGL unbinds a context current on the deleting thread. Already-unbound
  contexts are temporarily activated through their recorded DCs on that thread,
  then the previous binding is restored, including after failed activation.
  WGL rejects activation of contexts owned by other threads. A retained one-pixel
  [WGL pbuffer](https://registry.khronos.org/OpenGL/extensions/ARB/WGL_ARB_pbuffer.txt)
  supplies a DC on the original device and pixel format when application DCs
  have disappeared. The previous context is restored before releasing that DC.
  Pixel formats without pbuffer support still need a final-destruction fallback
  if every application DC is unavailable.
  Recovery allocation failures retry with a one-second backoff. A successful
  deletion postfix now removes any capture entries whose prefix could not
  activate the context and closes their viewer sessions. It retains unreleased
  native capture resources instead of running their GL destructors against an
  unrelated context; full reclamation in this fallback remains unresolved.
  Native regression coverage verifies failed activation leaves entries intact,
  confirmed deletion removes them, and the previous current context survives.
  Tracking metadata is invalidated only by the successful deletion postfix,
  including when no capture DC was recorded. Failed deletion preserves metadata;
  reused context handles cannot inherit deleted-context texture dimensions.
  Vulkan copy engines now enter the
  shared retirement queue on stream removal/disconnect and are polled without
  blocking presentation. Swapchain/device destruction explicitly waits. Binary
  presentation semaphores live in a separate swapchain-owned pool, so stream
  retirement cannot destroy them. Supported devices now negotiate the complete
  EXT surface/swapchain maintenance dependency set and retain per-swapchain
  presentation fences in that pool. Pool teardown waits for those fences; native
  WSI validation exercises this production retirement path. Existing application
  fences are borrowed and waited before returning, which adds a host wait in
  that case. An existing all-null fence list skips capture rather than replacing
  the application chain. Equivalent-batch allocation-error retries reuse the already
  submitted replacement waits and skip duplicate capture/publication. Retry
  state is scoped by queue, swapchain/image batch, and original waits. Reordering
  either list preserves the same pending attempt without changing downstream
  presentation order. It is cleared on enqueue or swapchain destruction.
  Pending attempts now retain strong native signal ownership across stream
  replacement and repeated enqueue failures. Successful retry enqueue transfers
  that ownership into the presentation fence; the producer owns this result and
  retirement sequencing. A uniquely matching subset of the failed image batch
  also reuses its pending signal. The active retry guard retains native ownership
  even if an omitted owner's destruction removes the registry entry, and can
  restore that ownership on another enqueue/setup failure. Destruction before
  guard acquisition removes only the destroyed owner's image pairs; surviving
  targets retain the signal, and the entry retires when no live targets remain.
  Overlapping expanded batches capture only newly added image pairs, waiting on
  the retained signal; another enqueue failure records the new final signal and
  expanded captured set. CPU regression coverage verifies this transition.
  Native combined presentation now exercises two swapchains. Full loader-hook
  expanded-batch and disjoint retry coverage remain pending. Unextended
  retries retain detached native signals when no completion proof is available;
  unextended shutdown, and full hook
  regression coverage remain to be completed; see
  the [Khronos semaphore lifetime guidance](https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html).
  Device setup now recognizes existing timeline feature nodes, including Vulkan
  1.2 features, and queries support before injecting a missing node. Explicitly
  disabled timeline support leaves capture unavailable while preserving the
  application's device-create chain, rather than appending conflicting features.
- OpenGL still supplies fallback-window/configuration glue. Stable resource
  history slots are integrated in all four graphics providers; Vulkan routing
  and OpenGL placement history now use the computational registry.
- Complete native application regression comparisons and the remaining failure,
  disconnection, retirement, and cross-window migration scenarios.

## Validation performed

- CPU-only core: `cargo test -p polymer-presentation --no-default-features --lib`
  passed (29 tests).
- Producer-feature core: 39 tests passed, including real local IPC handshakes,
  rejection, protocol mismatch, failed handle preparation, and independent frame
  numbering/synchronization values, plus setup retry/rejection/disconnection and
  independent sibling retirement. A real IPC disconnection fixture verifies
  publication failure cleanup and continued sibling frame numbering. Fake
  operations verify inline dispatch, busy/error outcomes, and no submissions
  through an inactive or disconnected backend. Routing
  fixtures cover equal extents, distinct layers/domains, page flips, resize,
  reordered sources, title precedence, expiry, and window/device destruction.
- Producer libraries passed: D3D11 (4), D3D12 (11), OpenGL (19), Vulkan (29),
  GDI (1 native paint/DC ownership, publication, and backpressure fixture).
  The GDI fixture negotiates an unsupported consumer and an incompatible
  protocol before a successful session, verifying neither declined negotiation
  publishes an image export.
- A native D3D11 WARP fixture forces fence signaling to fail through a foreign
  device context after copy submission. It verifies retained source/slot state,
  repeated failure, recovery without an unpublished consumer acknowledgment,
  and consumer-release gating after the next successful publication.
  It also checks that capture construction adds no swapchain reference and that
  dropping a capture after a failed signal releases held backbuffer references:
  a subsequent native ResizeBuffers succeeds after flushing queued commands.
- A native WGL context fixture verifies temporary activation, restoration after
  success/failure, rejection of cross-thread ownership, and capture-entry cleanup
  before deletion of an unbound context. Restoration preserves separate draw and
  read DCs using the application's WGL make-current-read entry points; the native
  fixture supplies a pbuffer read DC distinct from its window draw DC.
  It also destroys the application window
  and releases its DC before verifying pbuffer-based context activation, entry
  cleanup, and recovery-surface release.
  Under that current context, an EXT timeline cleanup check verifies both the GL
  semaphore and native fence handle become invalid when setup ownership is
  dropped before constructing a capture session.
- The WGL fixture also exercises PBO, NV, and EXT capture/publication with a real
  IPC viewer. It imports the advertised shared image through D3D11, acquires its
  keyed mutex, and verifies every pixel in a 16x12 crop from a padded 64x64 GL
  texture. Red/green rows verify bottom-up storage and blue padding stays outside
  the crop. The published timestamp precedes the publishing copy call, verifying
  delayed readback retains its original capture time. The NV case fills all
  consumer-owned slots and verifies that a busy ring skips capture. The EXT case
  imports the advertised D3D12 image and both fences, waits on the producer fence,
  and verifies every cropped pixel through GPU readback. It also fills the ring,
  verifies busy capture skips, and resumes capture after consumer-fence release.
  This fixture calls the adapters directly; full application comparisons remain.
- All five producer and hook crates passed `cargo check --all-targets`; their
  library suites and the IPC library suite passed together.
- D3D11 hook suite passed (7 tests), including real hardware capture/import,
  resize, keyed-mutex backpressure, and two unequal padded sources.
- D3D12 hook suite passed (2 tests), including real hardware submission of two
  unequal padded sources into independent streams and release before resize.
  The scenario imports each published image and synchronization fence, reads
  every pixel in both content regions, and verifies distinct red/green content
  without the blue allocation padding before releasing consumer ownership.
  A later scissored UI observation is injected at the recording boundary to
  verify that uncertain raster coverage does not override either exact source.
  This additional observation does not submit a shader draw.
  The fixture also verifies that ordinary selection consumes its observations,
  immutable command-list replay restores both sources, and graph selection
  retains both across repeated presentation observations.
  Two consecutive native instances run in one process. D3D12 supplies window
  liveness metadata and the shared registry immediately invalidates destroyed
  windows, so a still-retained old swapchain owner cannot reserve the new
  instance's output slots. A CPU fixture verifies surviving window assignments
  remain stable while the destroyed window's slots and ownership are released.
- A native Vulkan WSI fixture passed with `VK_EXT_swapchain_maintenance1`: it
  creates a Win32 surface and swapchain, acquires and transitions an image,
  presents using the shared semaphore pool, drops the stream, and waits for an
  actual presentation fence through the production completion object and pool
  retirement before destroying the swapchains. It presents two different-sized
  Win32 windows in one call using a shared wait signal and per-swapchain fences.
  The same fixture also passed through the rebuilt Vulkan layer DLL, with loader
  diagnostics confirming instance/device insertion and both swapchain hooks.
  `POLYMER_WSI_HOOK_NEGOTIATION=1` makes this test request only ordinary WSI
  extensions; that run verified the layer's maintenance negotiation. No viewer
  was connected. In this loader mode, cases without application-owned fences
  submit a null present chain and destroy swapchains through the hook before
  releasing their wait semaphores. This verifies automatic hook-owned fence
  insertion and retirement without a fixture fence or application WaitIdle.
  The fixture checks that the layer DLL is loaded before taking this path.
  A connected-viewer mode also passed through the loader: both windows negotiate
  D3D12 resource/fence transport, accept independent streams, and publish frame 1
  at their respective extents. The fixture checks distinct surface IDs and frame
  metadata, imports the advertised D3D12 images and fences, waits for each
  producer value, and checks every pixel against the red/green window contents.
  It signals consumer release after GPU readback and closes receiver-owned export
  handles. A padded graph-source mode also passed: it uploads red/green crops
  into blue-padded 64x64 images, blits each crop to a separate window, and verifies
  every imported pixel in the selected 32x48 and 24x32 outputs. The smaller exact
  source remains selected despite its width being below the uncertain-source
  filter. This exposed and fixed a logical surface-ID collision: positional
  requests now use registry-wide indices, while history slots remain scoped to
  their presenter. CPU coverage checks independent equal-sized sources, page
  flips, gaps, duplicate presentations, and window destruction. Title-qualified
  requests also resolve across presenters, using the same title-match policy
  as the other registry path. The native fixture passed with identical title
  patterns on both requests. CPU regressions cover title precedence, removal
  from the active candidate set without positional renumbering, and reordered
  unnumbered sources preserving both previous identities.
  Replacing one unnumbered source while reordering a survivor reserves the
  survivor's global index before assigning the replacement. The regression
  previously collapsed both entries into one output; both now remain selected.
  Representative before/after application comparisons remain pending.
  The fixture injects an enqueue allocation failure after a real GPU submission,
  then presents using the retained replacement semaphore without resubmitting
  work or waiting a second time on the consumed acquire semaphore.
  Both private and application-owned presentation fences are exercised. The
  borrowed-fence case verifies completion before return and then resets and
  destroys the still-valid application handle. An all-null application fence
  list is rejected before capture setup. Feature-negotiation tests also cover
  explicit maintenance enable/disable settings and unavailable feature queries.
  The native semaphore fixture also retains pending signals, destroys their
  streams and original pool, and then successfully submits and completes the
  final wait before releasing those retained references.
  A WSI variant transfers the native signal into its presentation completion,
  destroys the original pool, and waits for presentation with the completion
  object as the sole remaining signal owner.

The loader negotiation regression can be repeated from the workspace root in
PowerShell (these environment variables are for the test process):

```powershell
cargo build -p polymer-hook-vulkan-layer
$env:VK_LAYER_PATH = Join-Path (Get-Location) 'crates/hooks/polymer-hook-vulkan-layer'
$env:VK_INSTANCE_LAYERS = 'VK_LAYER_POLYMER_capture'
$env:POLYMER_VULKAN_LAYER = '1'
$env:POLYMER_WSI_HOOK_NEGOTIATION = '1'
cargo test -p polymer-producer-vulkan --lib native_presentation_fence -- --nocapture
```

For the connected-viewer variant, also set `POLYMER_WSI_VIEWER=1` and set
`POLYMER_SESSION` to a private test session name before running that command.
The fixture creates and owns the viewer listener for that session.
For padded graph sources, additionally set `POLYMER_WSI_GRAPH=1`,
`POLYMER_TRACK_RENDER_TARGET=1`, `POLYMER_TRACK_RENDER_TARGETS=0,1`, and
`POLYMER_GRAPH_TRACKING=1`. Rebuild the hook DLL after production code changes;
the producer test command alone does not rebuild the DLL loaded by Vulkan.
The identical-title regression additionally uses
`POLYMER_TRACK_RENDER_TARGET_SURFACE_PATTERNS` set to
`["Polymer Vulkan fence fixture","Polymer Vulkan fence fixture"]`.

The optional `test-support` feature provides one D3D12 readback utility used by
the OpenGL, Vulkan, and D3D12 hook fixtures. It requires `producer`; CPU-only graph
builds do not enable it. The previous provider-local readback copies were removed.
- A native Vulkan semaphore fixture passed: it submits a signal, retires the
  stream references, reuses the swapchain-owned semaphore in a new stream, and
  waits for the dependency chain to complete. This does not exercise WSI
  presentation or replace application capture/import comparisons.
- Strict Clippy passed for the shared crate with all features and targets.

An external Vulkan application regression now runs the installed SDK's `vkcube`
through the actual loader, negotiates IPC with a separate viewer process, imports
the exported D3D12 image/fences, and saves optional PPM readback. Its hidden window
is driven with paint messages; the process is bounded to 120 frames and 15 seconds.
The arguments and paint-driven rendering follow the
[Khronos cube application](https://github.com/KhronosGroup/Vulkan-Tools/blob/main/cube/cube.c).
The SDK 1.4.350.0 application passed on this machine: whole-frame mode produced
320x240, while graph mode produced its centered 240x240 viewport. Frame 1 of the
graph output matched that crop of the whole-frame output byte-for-byte.

This application run exposed two fixed errors. An uploaded sampled logo texture
was selected as a screen; uncertain candidates now require an observed raster
write or exact-copy provenance from one. Explicitly presented copies still do
not require a raster write. D3D11/D3D12 and Vulkan no longer publish synthetic
render-target observations for copy-only resources. Vulkan also incorrectly
blocked its backbuffer fallback based on the stream's logical-ID tag; that
duplicated heuristic and its tag parser were removed. CPU regressions and the
existing native D3D tests preserve copied render-target selection.

OpenGL now uses the same rendered-content evidence when constructing graph
targets and its ordinary-tracking fallback. Sampled asset metadata remains
available for exact mapped transfers without becoming an uncertain screen root.
The shared observation store keeps write recency separate from observed use:
static rendered content propagated through a copy remains eligible while used,
but a later smaller draw replaces its coverage score. Resource invalidation
removes that evidence immediately. CPU and OpenGL adapter regressions cover this
distinction, including exact copies with no raster write.

The remaining OpenGL selection fallback that ranked render targets without a
presentation connection was removed, along with the shared entry point used
only by that fallback. An unrelated rendered texture no longer overrides the
backbuffer fallback. OpenGL also no longer fabricates mip-0/layer-0 candidates
for raw presented texture names: copy/blit recording supplies measured native
subresource metadata. Regressions cover unrelated targets, presented sampled
targets, uploaded assets, and exact mapped mip/layer crops in both tracking modes.

Renderbuffers now have distinct graph identities and measured extents. Textures
and renderbuffers share one incarnation allocator keyed by object kind
and name, preventing registry identity collisions across the two namespaces.
Draws, copies, and blits record dependencies between the actual resources; the flattened
renderbuffer-input map was removed. Exact transfer mappings retain source crops,
destination coverage, and reflection through renderbuffer intermediates. The
adapter regression decomposes an unequal two-source composite through a scaled,
reflected renderbuffer and checks texture/renderbuffer namespace separation.
Native metadata queries preserve renderbuffer binding and avoid texture-only
attachment queries for renderbuffers. Capture revalidates lifetime, dimensions,
and single-sample eligibility.

Deletion is observed through both `glDeleteRenderbuffers` and its EXT entry point.
Reused names discard old graph evidence without invalidating an identically
numbered texture or another context's objects. The CPU regression covers that
lifetime boundary. An installed-hook native fixture verifies resolver routing
for both deletion aliases, forwarding to native deletion, zero-count calls, and
preservation of the same-numbered texture. It also allocates and deletes a new
renderbuffer afterward. This complements the CPU graph-invalidation regression;
the hook fixture does not directly inspect graph storage. The native hook test
and all 21 OpenGL producer tests pass.
The three OpenGL transports share source-subresource attachment in
`swapchain/source_attachment.rs`. Native fixtures now record an actual renderbuffer
blit, select its interior crop through the shared selector, revalidate the selected
resource, and capture/import it through PBO, NV, and EXT. All three pixel checks
pass on this machine. Transfer recording was split into `tracking/transfers.rs`.
Blit recording queries `GL_READ_BUFFER` and each enabled draw-buffer route.
The native fixture now reads color attachment one and checks that disabling all
draw buffers records no presentation. Its selected crop still passes all three
transport import checks. This follows the read-buffer and draw-buffer routing
defined in the [OpenGL specification](https://registry.khronos.org/OpenGL/specs/gl/glspec30.pdf).

Routing also reserves other presenters' logical indices when a previously shared
source diverges. Two windows may share an index for the same source; changing one
window's source now creates an independent index without displacing its sibling.
The regression covers a short observation gap and subsequent source reunion.

To repeat, use the loader environment above, set `POLYMER_APPLICATION_PATH` to
the SDK's `vkcube.exe`, and run
`cargo test -p polymer-producer-vulkan --lib external_vulkan_application -- --ignored --nocapture`.
Set `POLYMER_APPLICATION_CAPTURE` to an output PPM path to retain pixels. Use no
tracking variables for whole-frame mode; set `POLYMER_TRACK_RENDER_TARGET=1`,
`POLYMER_TRACK_RENDER_TARGETS=0`, and `POLYMER_GRAPH_TRACKING=1` for graph mode.
The test asserts the expected rendered extent to reject the previously selected
256x256 logo asset. It remains ignored in ordinary test runs because it requires
the external application and a rebuilt layer DLL.

A verified comparison against the pre-refactor Vulkan implementation is recorded
in [the application comparison](verification/vulkan-application.md). Whole-frame
pixels match exactly; current graph mode captures the centered viewport where
the baseline accepts a stream but publishes no frame. The fixture now optionally
verifies the loaded DLL path and retains loader stderr diagnostics. OpenGL
application comparison is recorded in
[the OpenGL comparison](verification/opengl-application.md). Whole-frame pixels
match the baseline; graph tracking now preserves FFplay's final color output
instead of selecting an uploaded grayscale video plane.

OpenGL transport operations, export publication, and native destruction now live
in separate EXT, NV, and readback modules; the swapchain module retains window
assignment and orchestration. Transport publication uses bound implementation
methods and the shared `ProducerSession`. Strict OpenGL producer Clippy, all 21
provider tests (including native texture/renderbuffer imports across the three
transports), and the OpenGL hook build check pass after this extraction.

D3D12 enhanced-barrier observations now preserve the affected mip, array, and
plane range instead of overwriting state for the entire resource. Split starts
invalidate capture eligibility until their completion is submitted. The adapter
regression covers unrelated subresources, reset/unsubmitted completions, queue
ownership, whole-resource overrides, and 3D depth versus array indexing. These
follow Microsoft's [subresource range definition](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_barrier_subresource_range)
and [split synchronization rules](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3d12umddi/ne-d3d12umddi-d3d12ddi_barrier_sync).
The native copy path now accepts supported enhanced-state sources and transitions
only the selected subresource to copy-source and back to its observed layout.
Legacy intermediates retain legacy transitions; implicit legacy-to-enhanced state
translation was removed, following the
[barrier interoperability rules](https://microsoft.github.io/DirectX-Specs/d3d/D3D12EnhancedBarriers.html#interop-with-legacy-resourcebarrier).
The enhanced-array fixture passed on this machine: two padded layer-1 sources
published independent 32x64 and 24x32 crops, imported pixels matched their colors,
and capture references were released before resize. It probes feature support
and reports unavailable hardware before starting a viewer. The original native
legacy-state multi-stream fixture also remains covered.

The repository's human-owned `ARCHITECTURE.md` was deliberately left unchanged;
its producer-crate inventory needs a human update for this new crate.

The shared retirement queue performs a final nonblocking readiness poll when
dropped. Resources that completed after the owner's last poll are released;
still-busy resources retain the existing safe-retention behavior. The regression
checks completed and busy siblings together and verifies that destruction does
not invoke the backend's wait operation. D3D12 and Vulkan producer tests pass
with this change; their readiness checks run before native device destruction.

The GDI hook now has an installed-detour native fixture. It creates a hidden
window, calls Win32 `BeginPaint`/`EndPaint`, and verifies the resulting stream's
window identity and first frame. A cross-thread `DestroyWindow` failure must
leave that stream connected; successful destruction on the owning thread must
close it. This complements the producer fixture's outstanding-DC retirement
checks. Both GDI fixtures and all 47 shared producer tests pass.

All five producers now publish a borrowed `ExportDescriptor` rather than build
`StreamReady` directly. The descriptor carries native format, adapter,
orientation, source window, and handle slices. Shared publication validates the
slice length before packing the protocol array, derives the slot count, and
supplies the producer process ID. Provider-local capacity checks and truncating
array/slot zips are removed. Native handles remain backend-owned; the existing
shared duplication/receiver-ownership rules and IPC wire format are unchanged.
The full producer suite passes (114 tests, two opt-in application fixtures
ignored), as do all 18 graphics/GDI hook tests, strict shared Clippy, and the
Linux CPU-only build check.

Placement pairing no longer lets equal source/observation counts override
ambiguous identities. Ordered page-flip inference requires unique identities on
both sides; proven identity matches survive independently. This prevents two
draws of one face from assigning a destination to an unrelated historical face,
or a partial identity from collapsing independently selected layers. CPU
regressions first reproduced the incorrect assignments, and the OpenGL repeated
face regression now expects unknown placement. All 47 shared producer, 21
OpenGL, and 28 Vulkan tests pass after this policy correction. Capture pixels
are unaffected; only unsupported presentation placement is withheld.

OpenGL fallback window selection now uses the registry's fresh, ordered window
snapshot instead of a separate process-lifetime HWND list. First-observation
ordering, owner deduplication, explicit destruction, and the 240-generation
freshness boundary are computational registry behavior. A regression covers
destruction and later reuse of a window identity. The existing title matcher
receives this snapshot, preserving CLI matching rules. All 38 CPU tests and 21
OpenGL tests pass, as does strict Clippy. A rebuilt hook's FFplay graph capture
remains byte-identical to `ffplay-current-graph.ppm` (new artifact:
`target/ffplay-registry-graph.ppm`).

Registry source ordering now breaks simultaneous equal-size observation ties by
stable owner/window identity before local index, so DashMap iteration cannot
choose output indices or duplicate ownership. Cached presentation routes expire
after 240 generations without supporting window evidence; the 600-generation
idle-retirement horizon no longer extends that ownership evidence. Regressions
reproduced both defects before the fixes and cover reversed owner insertion and
the exact route-expiry boundary. All 49 shared producer tests and the four
graphics-provider suites pass.

Routing now selects the newest fresh observation for each native window before
checking layout compatibility. A replacement swapchain with a different layout
cannot expose its predecessor as matching evidence. Shared-layout inference
also requires matching resource/subresource identities and their own extents;
one shared overlay plus equal-sized unrelated siblings is insufficient. Existing
logical-slot retention still handles page flips and short gaps. Both regressions
failed before the corrections. All 51 shared producer tests, 28 Vulkan tests,
and a rebuilt layer's native two-window graph capture/import fixture pass.
Current completion evidence and remaining review are tracked in the
[completion audit](verification/completion-audit.md). Historical test counts
below describe the state when each change was verified.
