# Completion audit

This audit tracks the original four-phase plan. It is not a completion claim.
The README contains chronological implementation notes; passing counts there
are snapshots, not substitutes for checking current code and behavior.

## Requirements and evidence

| Requirement | Current evidence | Audit status |
| --- | --- | --- |
| Computational crate without graphics/IPC dependencies by default | Default-feature CPU suite: 51 passing tests on 2026-09-07; Linux CPU-only all-targets build passed after selection-order fix | Verified for tested configuration |
| Object-safe surface metadata, graph IDs, guarded views, cached queries | Shared interface and four snapshot adapters; metadata/lifetime audit below | Verified interface and adapter metadata contracts; command recording and native retirement audited separately |
| Observation deduplication, generation retention, reset, and pruning | Shared observation storage and adapter fixtures exist | Recheck all recording paths against submitted work |
| Checked geometry, crop/coverage separation, mapping composition and orientation | Shared geometry audit below; native padded/cropped imports | Shared computational behavior verified; adapter recording coverage reviewed separately |
| One selector across GL, Vulkan, D3D11, D3D12 | All four adapters use the shared selector; OpenGL default-framebuffer metadata/crops now integrated | Implementation gap closed; final policy and application regression review remains |
| D3D11 deferred contexts; D3D12 lists, bundles, descriptors, barriers and queue ordering | CPU recording tests and native D3D capture/resize fixtures pass | Complete concurrency and reset/replay coverage audit |
| Resource history: identity first, compatible slots, invalid removal, exact shrink | See detailed history audit below | Verified shared behavior and integration call sites; native validation remains adapter-owned |
| History timing: 60 changed observations, 120 missing presents | `history/mod.rs` boundary test passes | Verified computational defaults |
| Registry identity domains, titles, ownership, geometry and deterministic ordering | Registry tests include domain isolation, insertion order, exact expiry, replacement observations, unrelated siblings, and ambiguous placement | Recheck native identity reuse and remaining provider glue |
| D3D multi-stream output and independent siblings | Per-stream capture/session state; shared title/index resolution; native unequal-source, rejection, D3D12 live-index-change, and resize fixtures | Verified integration and tested behavior; details below |
| Negotiation before allocation; acceptance; protocol/error distinctions | See session-publication audit below | Verified shared failure handling and all producer allocation call sites |
| Export descriptor, slot validation and duplication ownership | Borrowed `ExportDescriptor` used by all five producers; shared IPC/rollback tests pass | Verified migration; wire schema unchanged |
| Shared frame publication, numbering, timestamps and busy behavior | See frame-publication audit below; shared session and native producer suites pass | Verified current publication paths |
| Shared lifecycle and native-safe retirement | All five producers use `CaptureSession`; native GDI destruction and GL context tests pass | Outstanding retirement review described below |
| Shared scheduler and backend leases | Shared failure/fairness tests; native D3D, all GL transports, Vulkan opaque release, and GDI paint fixtures; details below | Verified current acquisition, failure bookkeeping, and reuse paths; retirement remains a separate audit |
| Representative before/after GL and Vulkan comparisons | [OpenGL](opengl-application.md), [Vulkan](vulkan-application.md) | Completed for documented applications/drivers; intentional changes recorded |
| Existing producer/hook checks and hardware import tests | Latest broad runs and targeted reruns recorded in README | Run final suite after all remaining implementation changes |

## Slot ownership audit progress, 2026-09-07

Native opaque Vulkan ring exhaustion/resumption now passes. A fixture drives the
production copy engine with three exported slots, waits for producer completion,
and proves all slots remain unavailable without consumer release. A separate
viewer imports and reads slot zero, signals release value one, and exits. The
next copy reuses only slot zero at synchronization value four; another attempt
remains busy because neither sibling was released. Busy capture leaves the next
synchronization value unchanged. This runs in each of the three WSI fence
scenarios (`target/vulkan-opaque-ring-native.log`). Viewer import code remains
outside the producer, in `polymer-test-viewer`; the producer fixture only drives
native capture and checks scheduling outcomes.

The scheduler audit now verifies the current acquisition and bookkeeping paths:
shared reservations commit ordering only after lease completion; D3D11/D3D12
record submitted work before fallible fence signals and avoid unpublished
consumer obligations; Vulkan distinguishes failed enqueue from pending work;
GL EXT checks consumer acknowledgement, NV uses WGL/keyed ownership, and PBO
uses keyed leases only for shared output slots; GDI retains paint/DC ownership
through its paired boundary. Shared failure/fairness tests and native busy/reuse
fixtures cover these contracts. Resource destruction remains in the separate
retirement audit; this finding does not close that item.

Viewer placement correction: WSI handshake/readback support and the opaque child
viewer now live in `crates/consumer/polymer-test-viewer`. The Vulkan producer has
only a dev-dependency on that support crate; its graphics fixture and external
application launcher remain producer tests. Build `polymer-test-viewer` before
the opt-in opaque fixture. Native cross-process import and teardown pass after
extraction (`target/vulkan-extracted-viewer-native.log`), the 38 Vulkan producer,
hook, and consumer library tests pass, and strict Clippy passes for the new
viewer crate. The child is now an executable rather than an ignored producer
unit-test entry point.

The opaque WSI fixture now forwards exports through `ProducerSession` to a
separate child process with capture-layer activation removed. This exposed a
consumer lifetime bug: `ImportedStream` did not retain its dynamically loaded
`ash::Entry`. It now owns that loader through native resource teardown. The
cross-process fixture passes all three presentation-fence scenarios with two
unequal streams each, checking every imported pixel and signaling consumer
release before teardown (`target/vulkan-opaque-child-native.log`). This replaces
the failed same-process experiment described below as the native import and
retirement evidence. Native opaque ring exhaustion/resumption remains to verify.

Inspected shared reservations and native acquisition/submission paths for D3D11,
D3D12, all three OpenGL transports, Vulkan, and GDI. The OpenGL native fixture
now exhausts PBO shared output slots as well as NV and EXT slots while retaining
consumer ownership. All three skip capture at capacity; the 23 OpenGL tests pass
with one external-application fixture ignored (`target/gl-all-transport-busy.log`).

Found and fixed an incomplete Vulkan opaque-handle path: only the D3D12 export
variant created a consumer-release timeline. Opaque Vulkan exports now create
and publish that timeline too, using the existing wire field and consumer import
support. Failure to create it prevents publication. Slot acquisition treats a
missing release timeline as value zero, so it cannot authorize reuse of a
previously published image. The submission-failure regression now covers this
missing-acknowledgement case. Producer, hook, and Vulkan consumer library tests
pass (`target/vulkan-opaque-release.log`). The Vulkan compositor already signals
imported consumer timelines after its accesses; no wire-format change is needed.

Native opaque-handle export/import and ring exhaustion still need an explicit
end-to-end rerun before closing this scheduler audit. The existing native WSI
fixture exercises the D3D12 consumer variant.

The WSI viewer now accepts `POLYMER_WSI_OPAQUE=1` to negotiate Vulkan and import
through `polymer-consumer-vulkan`. Both unequal streams import and their pixel
checks pass, including the consumer-release timeline. However, this opt-in
same-process variant is not passing: subsequent producer swapchain destruction
has hung or crashed. Instrumentation localized the failure after viewer
verification and before pool teardown. The hung test processes were terminated;
no validation process remains intentionally running. Logs are
`target/vulkan-opaque-native.log` and `target/vulkan-opaque-validation.log`.
Validation reported outstanding swapchain/command uses, but layer ordering also
produced a maintenance-feature mismatch, so those diagnostics need an isolated
rerun. Move the consumer to a separate process, as in production, then investigate
any remaining failure. Do not treat successful pixel reads as retirement proof.
The ordinary producer/hook suites still pass (31 plus 6 tests, one external-app
fixture ignored; `target/vulkan-opaque-fixture-suite.log`).

## D3D11 resolve requirements and current suite, 2026-09-07

D3D11 capture configuration now distinguishes cropped output requirements from
the full native allocation required by a multisample resolve. Equal crops no
longer allow reuse of a missing or differently sized resolve texture. Sample
count changes within multisampled sources need no rebuild when the native
extent matches: the reusable resolve destination is always single-sampled.

The native WARP crop fixture now runs through the shared capture lifecycle with
single-sample, multisample, larger multisample allocation, then single-sample
sources. Each source uses array layer 1 and the same 16-by-20 crop. Configuration
changes rebuild resources; unchanged requirements preserve them. Readback checks
every pixel for the selected color and excludes surrounding allocation padding.
This is reference-driver validation, not physical-hardware MSAA validation.

The combined shared producer-feature, five producer, and five hook library
suites pass: 157 tests, two external-application fixtures ignored. The separate
CPU-only suite passes all 51 tests. Logs: `target/presentation-current-suite.log`,
`target/presentation-current-cpu.log`, and
`target/d3d11-resolve-reconfiguration.log`. These runs do not close the remaining
identity, recording, retirement, and lease audit items above.

## Selection-history audit, 2026-09-06

Inspected `src/history/mod.rs`, `src/history/resources.rs`, the four adapters'
`HistorySurface` implementations, and their capture reconciliation call sites.

- `ResourceHistory::reconcile` revalidates both held and observed resources before
  selecting them. Invalid entries become holes immediately and emit removal
  changes without shifting surviving logical slots.
- Observed identities are reserved before held resources, preventing equal-size
  siblings from selecting the same resource. Slot assignment matches surviving
  identities before compatible live or historical slots.
- An exact selection returns its observed resources without historical filling.
  The exact-shrink regression verifies replacement/removal behavior.
- GL identity includes incarnation, target kind, mip and layer. Vulkan includes
  incarnation, mip and layer. D3D history retains COM-backed resources and uses
  resource identity plus subresource; compatible layouts also include format.
- GL calls `refresh_sources`, Vulkan calls `refresh_source`, D3D11 calls
  `revalidate`, and D3D12 calls `refresh_presentation` with the submitting queue.
  These checks remain native adapter responsibilities, not authority granted by
  selection history.
- Reorder, page flip, invalid removal, exact shrink, 60-observation adoption and
  120-present missing-selection boundaries pass in the current CPU suite.

## Observation generation audit, 2026-09-06

D3D12 root-signature reuse follow-up, 2026-09-07: creation clears any prior
layout at the returned native identity before deserialization. An unsupported
or unreadable new signature therefore remains unknown instead of reusing old
table ranges. The existing 14 producer and 4 hook tests pass
(`target/d3d12-root-reuse.log`); these tests do not specifically inject a valid
native signature that the adapter's deserializer cannot read.

D3D12 descriptor reuse follow-up, 2026-09-07: command-list hook lifetime already
clears abandoned state on final release. Newly created descriptor heaps now
clear cached SRV/RTV entries within their CPU address range and invalidate
overlapping GPU heap mappings before recording the new shader-visible mapping.
CPU-only heaps are included; their GPU handle is not queried. The descriptor-copy
regression proves that recycled unwritten slots stay unresolved while unrelated
heap entries remain intact. All 14 producer and 4 hook tests pass
(`target/d3d12-descriptor-reuse.log`).

D3D11 deferred-context lifetime follow-up, 2026-09-07: context hook state now
shares an explicit lifetime guard across its interface clones. Its final drop
removes deferred bindings and unfinished observations. Creating a deferred
context also starts with fresh state instead of retaining an entry at a reused
address. Finished command-list batches remain independently owned and executable.
The deferred-copy regression verifies unfinished work/bindings are discarded,
recreated context state is empty, and a surviving finished list still publishes.
All 6 D3D11 producer and 8 hook tests pass
(`target/d3d11-context-retirement.log`). This closes a concrete recording-lifetime
gap; the remaining recording-path audit is still open.

Inspected OpenGL `take_draw_candidates`, Vulkan `take_present_sources`, and both
D3D `take_swapchain_presentations` paths. All advance the recording generation
before snapshot selection. Thus the shared inclusive lower cutoff `now - 8`
retains eight completed presentation generations, not nine. Ordinary tracking
clears observations after selection in all four adapters. Mid-recording store
queries can also see the in-progress generation; the store contract now makes
that distinction explicit. A CPU regression checks the exact expiry boundary
and that replay refreshes only the replayed source. All 47 CPU tests pass.

## Frame-publication audit, 2026-09-06

Inspected every producer's `CapturedFrame` construction, `CapturedFrame::present`,
`FramePublisher`, and `ProducerSession::send`.

- D3D11, D3D12, Vulkan, GL EXT/NV, and GDI stamp successful submissions before
  publication. Busy/skipped paths return without constructing a frame.
- GL PBO records QPC beside each queued readback and returns the completed
  readback's timestamp. Its native texture/renderbuffer fixture asserts that
  this timestamp precedes the later publication call while importing pixels.
- `CapturedFrame::present` preserves the supplied timestamp and synchronization
  value. The session assigns only the publication number, advances it only on a
  successful send, and rate-limits successful-frame diagnostics to the first
  frame and every 120 frames. Failed sends deactivate and release that session.
- Shared IPC tests verify independent sibling numbering, send failure,
  rejection, unchanged capture clocks, and GPU values distinct from frame
  numbers. The current 57 shared tests and all five producer suites pass.

## Session-publication audit, 2026-09-06

Inspected shared session/export code and the allocation/publication paths in
D3D11/D3D12 capture, Vulkan `create_transport`, GDI `Capture::new`, and OpenGL
`CaptureSetup` plus EXT/NV/PBO constructors.

- All negotiate `Hello` and activate the frame channel before native output-ring
  allocation. D3D11/Vulkan choose transport from the consumer preference; GDI
  rejects unsupported consumers before device/ring allocation. OpenGL negotiates
  before selecting and allocating its available native transport.
- Protocol mismatch and rejection are classified separately from connection
  failure. Shared `setup_result` parks the former; transient errors reach the
  lifecycle backoff. OpenGL constructors can return an allocated capture with
  no accepted IPC session; shared initialization checks `connected()` and parks
  it before any copy operation.
- Publication accepts only `StreamAccepted`; closure after `StreamReady` is
  rejection, while timeout/reset remains retryable. Frame sends require an
  accepted session.
- Export preparation validates slot counts and owns remote duplicates until
  publication starts. Preparation failures close those duplicates while keeping
  borrowed originals usable. Once publication starts, receiver ownership is
  retained even on rejection or partial-write failure, avoiding a close/import
  race. Native transport allocation and format conversion stay in providers.
- Current shared IPC fixtures cover protocol mismatch versus missing viewer,
  explicit/unacknowledged rejection, sibling isolation, slot limits, duplication
  rollback, and accepted frame delivery. All 57 shared producer tests pass.

## Shared lifecycle audit, 2026-09-06

Inspected `producer/lifecycle/mod.rs` and its operation, configuration,
rejection/backoff, and sibling-isolation fixtures.

- Initialization is synchronous on the calling thread. Transient failure retains
  its attempt clock and cannot retry before one second. Rejection parks until a
  source or configuration change; unchanged requirements preserve that state.
- Compatible source page flips leave active resources intact. Configuration
  changes invalidate only that session's backend, then allow initialization with
  the new requirements.
- Capture checks consumer connectivity before dispatch and again after the
  operation. Inactive consumers cause no native submission. Busy returns, native
  errors, and successful operations have separate counters; publication failure
  parks the affected stream without disturbing its sibling.
- Invalidation moves active ownership through the retiring state and invokes
  backend destruction on the caller's thread. This verifies shared sequencing,
  not native completion: the backend must defer destruction of in-use GPU/DC
  resources. `RetirementQueue` polls completion, retires ready siblings
  independently, and retains unfinished native ownership if its owner vanishes.

The shared lifecycle fixtures and retirement fixtures pass in the current
producer/CPU suites. Native synchronization and teardown boundaries remain
under review below; the lifecycle integration row is deliberately still open.

## Remaining retirement review

Window identity audit, 2026-09-07: `ProducerWindows::refresh` validates only
process ownership of currently observed HWNDs. It cannot detect destruction
followed by reuse between refreshes. `PresentationRegistry::forget_window`
already invalidates owner and routing state, but only GDI currently intercepts
`DestroyWindow`; graphics-only injection needs a common notification path.
Do not substitute an unmanaged `SetPropW` marker: the documented contract
requires removing added properties during `WM_NCDESTROY`
([SetPropW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setpropw)).
The next implementation step is shared native destruction notification with a
test that delivers destruction and replacement before the next presentation
refresh. Keep native notification ownership outside the CPU registry, and
verify callback/module lifetime and cross-thread delivery before integration.

D3D11 multisample crop validation: added a native WARP fixture with a 64x64,
two-sample, two-layer texture. Layer one contains a red 16x20 rectangle at
(8,12) surrounded by blue padding. Capture resolves subresource one and copies
the selected crop; staging readback verifies every output pixel is red and
the output is 16x20/single-sample. This verifies the resolve/crop sequence and
nonzero array-layer routing on the reference driver. All six producer and
eight hook tests pass (`target/d3d11-msaa-crop-validation.log`). Hardware-driver
multisample coverage is not established by the WARP test.

D3D11 multisample crop implementation: removed the adapter exception that
discarded selected source crops for multisampled textures. Capture allocates a
private single-sample resolve texture at the native allocation size, resolves
the selected subresource into it, and copies the selected region into the shared
output. Backbuffer content regions can use this path too. The private resolve
texture is reused only in the same immediate-context command stream and is
never exported to consumers. All five producer and eight hook tests pass
(`target/d3d11-resolve-crop.log`), but a targeted native multisample crop/pixel
fixture remains required before treating this new path as verified.

OpenGL backbuffer application regression: rebuilt the hook and reran the private
FFplay graph fixture. The first run exposed a changed EXT `bottom_up` flag
because a selected backbuffer now occupies a source slot. Configuration now
marks framebuffer-zero sources bottom-up even when cropped. The rebuilt rerun
publishes 320x240, 99 colors, bottom-up orientation and full-window placement.
`target/ffplay-backbuffer-graph.ppm` matches the previous current capture
byte-for-byte (SHA-256
`9b144471613d61c927d6adaa2aa17e50d2ba826e777c68ee706bfa86b0d95be3`).
Log: `target/ffplay-backbuffer-graph.log`. Strict OpenGL Clippy passes after the
orientation correction and drawable-identity changes.

OpenGL backbuffer fallback implementation: presentation supplies current
drawable dimensions to the graph view, and default-framebuffer draws record
independent raster coverage. Shared selection now returns an observed-content
crop or the full drawable when no offscreen source is selected. Backbuffer
records retain crop, placement, allocation dimensions, and a context-allocated
drawable identity distinct from texture/renderbuffer identities. Revalidation
checks current drawable identity and dimensions. EXT, NV, and PBO bind
framebuffer zero for these sources while retaining the selected source origin;
resource attachment is skipped. Disabled tracking retains whole-window capture.
An adapter fixture checks both tracking modes. Native 64x64 pbuffer fixtures
read a 16x12 red/green crop at (4,8) through every transport and verify imported
pixels, including delayed PBO metadata and EXT import after producer teardown.
All 23 OpenGL producer tests, the hook test, and strict Clippy pass (the latter
two ran before the final drawable-identity allocation adjustment). Output:
`target/gl-backbuffer-selector.log`. The previous fallback gap is implemented;
representative application regression checking remains to be refreshed.

OpenGL fallback integration gap: `SelectionView::PRESENT` has no surface
metadata in the production graph view. Default-framebuffer draws record sampled
dependencies but not an independent raster observation when no sampled resource
is available. Consequently shared backbuffer fallback cannot resolve a source;
`swapchain/mod.rs` instead chooses a whole-window capture outside the selector.
The original policy requires observed content bounds before full-backbuffer
fallback. Completion therefore requires adding default-framebuffer metadata and
raster evidence to the adapter, carrying the selected crop into EXT/NV/PBO
capture while binding framebuffer zero, and testing padded/default-framebuffer
content imports. Existing `source.is_some()` framebuffer decisions in all three
transports must distinguish a backbuffer crop from an attached resource. This
is an implementation gap, not merely missing hardware validation.

D3D11 deferred submission transaction: `ExecuteCommandList` now forwards the
native call and publishes its immutable observation batch under one shared
observation transaction. The native restore-state argument is forwarded
unchanged. The unequal-source hook fixtures now create a real deferred context,
record both clears and cropped copies, finish the list, and execute it through
the hook before presenting. The accepting and sibling-rejection scenarios use
both restore-flag values and publish the expected independent extents. All five
producer and eight hook tests pass; output is in
`target/d3d11-submission-transaction.log`.

D3D12 submission-order fix: replaced the queue's postfix observation publisher
with an infix that holds the observation transaction across the native
`ExecuteCommandLists` call and batch publication. Concurrent submissions can
no longer enter native execution in one order and publish observations in a
different order. The presentation-boundary fixture now verifies native
submission has not occurred while the boundary holds its guard, observations
remain absent during native submission, and the committed batch enters the
new generation. Replay still refreshes observation order. All 14 producer and
four native hook tests pass; output is in
`target/d3d12-submission-transaction.log`.

Graph interface and metadata audit: `Surface` exposes fallible extent through
an object-safe borrowed interface. `PresentationGraph` supplies IDs, targets,
incoming dependencies, separate resource identity, and mip/layer identity.
The selector caches successful and missing metadata per node. D3D snapshots
own cloned COM resource records; descriptor-derived subresource metadata remains
stable for those immutable allocations. OpenGL snapshots use context-scoped
incarnations and separate texture/renderbuffer namespaces, and held sources
are remeasured on the presenting context through DSA or restored binding-based
queries. Vulkan snapshots use creation metadata with incarnation and shape
validation; native revalidation also checks current layout and crop bounds.
Deletion/reuse, mip/layer separation, missing metadata, and query caching have
targeted fixtures. Selection does not bypass adapter capture eligibility or
queue/layout/synchronization checks. This closes the interface/metadata row;
recording completeness and resource retirement are separate requirements.

Vulkan target-only destruction follow-up: image removal now visits target IDs
as well as edge endpoints when clearing shared observations. A fixture records
two target-only images, destroys one, and verifies immediate removal without
disturbing the sibling. All 31 Vulkan producer tests pass; output is in
`target/vulkan-target-retirement.log`.

Vulkan swapchain shape and lifetime follow-up: the creation observer now passes
`imageArrayLayers` into capture state, and acquired swapchain image metadata
records one mip plus the declared layers. Metadata is recorded independently
of capture selection being enabled. Removing a swapchain image now clears its
surface incarnation, shapes, layouts, and dependency evidence as well as its
extent. A capture/tracking fixture verifies layer one of a two-layer swapchain,
rejects layer two, destroys the swapchain, and rejects the held source after
the image handle is reused. The 30 producer and six hook tests pass; output is
`target/vulkan-swapchain-layers.log`. This closes the swapchain shape gap noted
in the earlier bounds audit below.

Vulkan subresource metadata audit: `surface_extent` now rejects mip/layer IDs
outside the recorded image shape before returning dimensions. Selection and
held-resource revalidation share this check. The existing array/reuse fixture
now checks the valid last mip/layer and both out-of-range boundaries. All 29
Vulkan producer tests and six hook tests pass; producer output is in
`target/vulkan-subresource-bounds.log`. Ordinary images receive shape metadata
from the image-creation observer. Swapchain shape recording still needs review
for non-default `imageArrayLayers`; absent shape metadata permits only the base
mip and layer rather than inventing additional valid subresources.

OpenGL no-DSA revalidation: replaced the fallback that retained resources based
only on cached incarnation/crop data. It now checks native texture existence,
temporarily binds the texture, queries mip dimensions and array depth, and
restores the prior binding. Both query paths reject incompatible allocations
and removed layers. A forced-fallback native fixture verifies a live texture,
binding restoration, and rejection of mismatched cached dimensions. The CPU
fixture now rejects resources when no live metadata is available. OpenGL's
22 producer tests, the extended native context fixture, and strict Clippy pass.
This closes the unchecked fallback identified in the preceding array audit.

OpenGL array revalidation: direct-state-access metadata checks now query array
depth in addition to width/height and reject held layers outside the current
allocation. Previously a depth-only texture reallocation could retain a removed
layer. A query-backed fixture keeps layer zero while removing layer one at
unchanged 64x64 dimensions. All 22 OpenGL producer tests and strict Clippy pass
(`target/gl-array-revalidation.log`). The no-DSA metadata fallback still needs
review before closing the adapter resource-validation contract.

OpenGL EXT export-lifetime validation: the native texture/renderbuffer fixture
no longer calls `glFinish` before teardown. After verifying consumer slot
release, it submits another frame, flushes, drops the producer capture object,
and imports/verifies that frame through the receiver-owned export handles.
The native context test and strict OpenGL Clippy pass; output is in
`target/gl-retired-export-audit.log`. This exercises import after producer
destruction without an explicit completion wait; it does not force the GPU to
remain busy at the instant of destruction. Reviewed EXT memory/semaphore
deletion against the [Khronos extension specification](https://registry.khronos.org/OpenGL/extensions/EXT/EXT_external_objects.txt);
driver-managed GL references and receiver-owned native handles remain distinct
from shared lifecycle bookkeeping.

D3D multi-stream completion audit: inspected both `capture.rs` implementations
and shared registry/lifecycle contracts. Each requested logical stream has its
own `CaptureSession`, backend resources, accepted connection, frame counters,
and parked/retry state. Registry requests receive the configured indices and
native window-title matcher; shared fixtures verify title precedence over
positional requests and established assignments. Reconciliation reports stable
slots; source changes wake parked entries while compatible active page flips
preserve native resources. Configuration compares each entry's requested index
and dimensions/format, so unchanged siblings are not invalidated. Native D3D11
and D3D12 fixtures verify unequal padded screens and independent rejection;
D3D12 also imports both selected pixel patterns, exercises array layers,
changes indices live, and resizes. No single-source capture state remains in
these paths. The combined current shared/producer/hook run passes 152 tests,
with two external-application fixtures opt-in. Full output is
`target/presentation-integration-audit.log`. Other open rows are not closed by
this multi-stream evidence.

D3D12 live request validation: the native rejection fixture now changes
requested indices from `0,1` to `1,0` before resizing. Both source allocations
remain 64x64, so native dimensions/format alone cannot invalidate the parked
entry. Both streams reconnect, keep their logical surface IDs, and import the
correct pixels at the newly selected content extents. Capture after resize also
succeeds. All four D3D12 hook tests pass; logs are
`target/d3d12-live-request.log` and `target/d3d12-live-request-suite.log`.
This supplies the native live-index-change evidence missing in the earlier
request-configuration audit below.

D3D request configuration audit: both providers now include the requested
surface index in each capture session's configuration alongside native capture
dimensions/format. Previously a request-index change between equal-sized
resources could preserve a rejected session's parked state. Shared configuration
invalidation now applies to the affected entry; sibling entries with unchanged
requests and native requirements retain their state. Existing shared lifecycle
configuration tests cover parked-state invalidation. All 19 D3D producer tests
and all 12 D3D hook tests pass (`target/d3d-request-config.log` and
`target/d3d-request-config-hooks.log`). A native live-index-change scenario has
not yet been exercised by these tests.

D3D12 native rejection coverage: the padded unequal-source scenario now rejects
the first stream after export publication and verifies that the accepted sibling
publishes independent frame/synchronization value 1 and imports the expected
green pixels. Export allocation dimensions may include padding; `FrameReady`
retains the selected content extent. Receiver-owned handles are closed for both
streams, resize succeeds, and the resized 80x48 stream publishes successfully.
All four D3D12 hook tests pass (`target/d3d12-native-rejection.log`). This closes
the native sibling-rejection test gap noted in the earlier D3D11 audit below.

D3D11 native rejection coverage: the unequal padded-source hook fixture now
runs an additional scenario in which the viewer closes the first stream after
`StreamReady` without accepting it. That session parks and the independent
24x32 sibling still publishes frame 1 with synchronization value 1. Both streams'
published export handles remain receiver-owned and are closed by the fixture.
All eight D3D11 hook tests pass. Logs: `target/d3d11-native-rejection.log` and
`target/d3d11-native-rejection-suite.log`. Native D3D12 rejection coverage remains
to be added; the shared lifecycle sibling-isolation tests are not a substitute.

Native window validation audit: D3D11, D3D12, and OpenGL call
`ProducerWindows::refresh` before resolving requests. It validates process HWNDs
without holding native-call registry guards, then forgets destroyed windows.
A native fixture creates two windows, destroys one, and verifies immediate
ownership release while preserving the live sibling and unspecified-window
metadata. Destroying the sibling then removes it on refresh. The targeted test
and strict shared Clippy pass. Vulkan supplies surface/swapchain/device teardown
notifications through its own metadata adapter. This does not prove detection
of a same-process HWND destroyed and reused entirely between refreshes; that
identity-reuse case remains part of the open adapter audit.

OpenGL policy-removal audit: removed present recording's local maximum-score
sampled-texture choice. Recording retains every distinct sampled subresource
for the shared selection view; candidate deduplication now includes mip and
incarnation through `TargetKey`. Recovered presentation quads are associated
only with an unambiguous single sampled resource instead of an arbitrary winner
among equally scored bindings. A recording fixture checks multiple candidates,
duplicate bindings, ambiguous placement, and separate mip metadata. All 21
OpenGL producer tests and strict Clippy pass, including the native context and
transport import fixture. Results: `target/gl-candidate-audit.log`.

Shared geometry audit: inspected `geometry.rs` and `selection/geometry.rs`.
Rectangle intersection uses widened signed bounds and checked narrowing;
mapping uses widened integer arithmetic and outward rounding. Path composition
clips each known source, raster coverage, and destination extent, then maps
clipping back through inverse prefixes to the original source crop. Reflections
compose independently on both axes. All supporting mappings must agree;
missing mappings, conflicting placements, and exhausted traversal budgets keep
placement unknown. Scissor/coverage alone cannot establish a source crop.
Existing padded, conflicting, intermediate clipping, reflection, and budget
fixtures are supplemented by a two-reflection, two-scale regression with
downstream clipping, followed by removal of an intermediate mapping. Its
expected crop is computed independently from the affine mappings. The current
61 shared producer-feature tests and strict shared Clippy pass; output is in
`target/geometry-completion-audit.log`. This verifies the computational engine,
not the completeness of every native API recording path.

Explicit-source ordering audit: the selector now derives fallback order from
dependency observations for sources without raster-write targets. Previously,
equally ranked explicit sources could follow adapter iteration order instead.
Root ordering is established before ordinary tracking applies its output limit;
graph tracking uses the same stable source order for its final size sort.
Ordering metadata does not create draw coverage or rendered-content provenance.
A CPU fixture supplies reversed copy observations without target writes and
checks both tracking modes and limits, then changes them to sampled evidence
and verifies backbuffer fallback. All 50 CPU tests and all five producer suites
pass; strict shared Clippy and CPU-only Linux all-targets checking pass.
Producer results are recorded in `target/selection-order-producers.log`.

Delayed PBO geometry audit: readback previously paired an older PBO's pixels
and timestamp with the newest observation's placement. Each queued PBO now
stores its capture-time `FramePlacement` alongside its timestamp and returns
both when that PBO is uploaded to a shared output slot. Busy output capacity
still publishes no frame. The native context/import fixture changes placement,
reflection, and window dimensions during pipeline warmup and verifies the
published frame retains the original values and capture timestamp while the
viewer imports the expected pixels. All 20 OpenGL producer tests pass (one
external-application test remains opt-in); strict producer Clippy passes.

D3D subresource identity audit: D3D11's graph previously reported a flattened
subresource index as its layer, and both D3D histories used that flattened index
for layout compatibility. Both adapters now cache the descriptor-derived
`(mip, layer)` when recording a resource and use it in graph and history views.
This distinguishes equal flattened indices with different mip/layer meanings
and permits compatible layers across resources with different mip counts.
D3D12 selection no longer repeats `GetDesc` for this metadata. The D3D11
deferred-clear fixture now checks view and raw-copy subresource decoding.
All 5 D3D11 and 14 D3D12 producer tests pass, together with all 7 D3D11 and
3 D3D12 hook tests (including native import, array streams, and resize).
Logs: `target/d3d11-subresource-audit.log` and
`target/d3d12-subresource-audit.log`.

Obsolete-policy test cleanup: removed OpenGL's test-only `selection_identity`
ordering implementation and three tests that sorted its tuples without calling
production selection/assignment. Registry-call tests now use plain source IDs.
Shared registry fixtures exercise actual global ordering, page flips, gaps and
identity-preserving assignment instead. All 20 remaining OpenGL producer tests
and all 17 shared registry tests pass; the count reduction removes parallel
test policy, not production behavior.

Metadata-cache audit: `PresentationGraph::surface` returns an object-safe
borrowed `Surface`; selector-local extent storage records both known and unknown
results once per visited ID. The counting fixture verifies duplicates do not
repeat queries and missing metadata cannot become a candidate. D3D snapshots
own cloned COM-backed resource records; GL/Vulkan snapshot CPU metadata on their
selection paths and retain native validation responsibilities before capture.
Vulkan now deduplicates IDs before adapter metadata lookup and uses the resulting
snapshot extent when producing selected sources. Removed its unused legacy
presentation-list parameter. All 29 Vulkan tests and the layer build check pass.

Current-layer hardware rerun: rebuilt Vulkan and exercised the installed layer
with graph tracking, two swapchains, separate importing viewer, and presentation
fences. The fixture passes and publishes unequal 32x48/24x32 crops
(`target/vulkan-current-wsi-audit.log`). Rebuilt OpenGL's hook and reran graph-mode
FFplay import after the observation/score/retirement changes; its 320x240 color
capture remains byte-identical to the documented whole-frame reference
(`target/ffplay-current-audit-graph.ppm`, corresponding test log).

Vulkan unfenced-retention correction: presentations without maintenance fences
previously forgot a fresh vector of semaphore references on every present.
The semaphore now atomically retains one reference on its first unproven
presentation; subsequent uses release their temporary vectors normally. This
preserves lifetime safety while bounding repeated-present retention per native
semaphore. The native pool fixture verifies ten requests add exactly one
reference and reclaims its test retention only after an explicit completed
queue wait. All 29 Vulkan tests pass. Semaphore reclamation without a WSI
completion proof remains a separate exceptional retention limitation.

Shared scheduler audit: acquisition probes each slot once in round-robin order;
busy/failed attempts do not alter the cursor. Inline and detached reservations
advance only after backend handoff succeeds. Extended fake-lease coverage now
checks inline finish failure and pre-submission abandonment, alongside detached
failure and successful rotation. D3D11/D3D12/Vulkan adapters supply native
producer/consumer checks, GDI detaches ownership across painting, and GL PBO
uses the scheduler only for shared outputs while preserving its readback
pipeline. All 49 CPU tests and strict shared Clippy pass. These checks verify
common scheduling, not unresolved native teardown guarantees.

NV interop teardown correction: ring destruction now checks object
unregistration before deleting its GL name. Failed registrations retain their
native slot ownership and leave the interop device connection open. Previously
unregister failure was ignored and the device was closed even if a failed
unlock left an object registered/locked. The
[extension specification](https://registry.khronos.org/OpenGL/extensions/NV/WGL_NV_DX_interop.txt)
explicitly permits unregister failure for locked objects. All 23 OpenGL
producer tests and strict Clippy pass; targeted unregister-failure injection
remains to be added before considering this failure path verified.
Follow-up: extracted the ring's registration-retirement pass so a native fixture
can inspect retained ownership and retry cleanup. The fixture injects failed
unregistration for real registered textures, confirms their GL names remain
live, unlocks the object, restores native unregistration, and verifies complete
cleanup. Locking alone did not force unregister failure on this driver, so the
failure return is explicitly injected. All 23 OpenGL tests and strict Clippy
pass. Final Drop still intentionally retains ownership if retry cannot succeed.

OpenGL context-boundary audit: retirement runs under the presentation lock and
temporarily activates the original context using an observed DC or a retained
device/pixel-format-compatible pbuffer. The binding guard preserves separate
read/draw DCs and restores the prior context after attempted activation.
Capture cleanup precedes native deletion when activation succeeds. If it cannot
activate, capture entries remain intact until the native result is known; only
successful deletion invalidates resource tracking and remaining ownership.
The successful-deletion fallback disconnects and retains native capture objects
rather than running their GL destructors in another namespace. Native context
activation/restoration, cross-thread ownership refusal, recovery after loss of
the application window, and the installed hook fixture pass. Permanent loss of
all usable cleanup drawables remains an exceptional resource-retention limit.
This checks context affinity; transport synchronization is audited separately.

GDI paint-boundary audit: `begin_paint` recovers pending DC release before
configuration changes and refuses preemption while an active paint exists.
`EndPaint` checks HWND, DC and owning thread, then releases the DC before
signaling or publishing. Failed release moves the lease into pending ownership;
recovery refuses another thread. Consumer disconnect cannot drop the capture
while active/pending paint ownership exists. Successful window destruction
disconnects immediately and retires only after those leases clear. Submitted
and published fence values remain separate for retry and consumer backpressure.
Both the native producer and installed hook fixtures pass, covering two
injected release failures, cross-thread recovery rejection, backpressure,
destruction during painting, and HWND reuse. If the owning GUI thread never
returns or ReleaseDC permanently fails, process-lifetime state retains ownership
and declines subsequent capture rather than destroying an outstanding DC. This
is a documented exceptional retention limit, not a claim of eventual recovery.

Transfer-score correction: exact transfers still propagate known rendered
content, but no longer assign their destination area as a draw-coverage score.
This avoids turning a copied clear or padded allocation into strongest-draw
evidence. Explicit geometry remains on the dependency edge. The regression
checks propagation from a zero-score write and preservation of an actual
same-generation draw score. All 59 shared producer tests and all four graphics
producer suites pass. Prior clear-evidence changes also passed OpenGL/Vulkan,
strict shared Clippy, and the CPU-only Linux build.

Clear-evidence correction: clears now have a distinct shared evidence kind.
They establish target writes with zero draw-coverage score; their explicit
rectangles remain recorded but do not become dependency/mapping or visible
content-bound evidence. This prevents clearing an entire padded allocation
from overriding a narrower actual draw. The shared regression checks ordinary
and graph selection, and both D3D clear fixtures retain their subresource and
rectangle checks. All 58 shared producer tests pass. The combined D3D run timed
out in the D3D11 fence-consumer fixture (`target/clear-evidence-audit.log`);
that validation must be resolved before claiming the combined suites pass.
Resolved: the fence fixture omitted DXGI factory-detour installation and relied
on another test running first. It now installs its prerequisites explicitly;
the isolated fixture and combined D3D suites pass. This was independent of the
clear-evidence policy change.

D3D12 clear-recording correction: installed `ClearRenderTargetView` observation
uses the recorded RTV subresources and explicit clear rectangles, clipped with
shared geometry. It records a target write without shader dependencies or
inherited viewport/scissor. Evidence is published only on command-list execution.
The regression covers descriptor replacement after recording, signed clipping,
subresource identity, replay freshness, and reset. All 14 producer tests and
three native hook tests pass (hook output in `target/d3d12-clear-audit.log`).

D3D11 clear-recording correction: added the installed `ClearRenderTargetView`
hook and shared render-target-view decoding for draws/clears. Clear writes use
the existing observation batch machinery, with coverage from the selected mip
and array layers rather than bound viewport/scissor. The previously failing
hardware fixture now captures 48x64 and resizes successfully. A WARP deferred
clear fixture verifies mip 1/layer 1, no evidence before execution, and refreshed
order on replay. All five producer and seven hook tests pass
(`target/d3d11-clear-audit.log`), superseding the failed run documented below.
The deferred-clear regression additionally verifies disposal removes the
immutable batch and finishing an empty subsequent command list does not replay
the previous clear. Its single observed target is mip 1/layer 1; siblings remain
unobserved. The expanded targeted test passes.

D3D11 ownership/resize audit: fence-backed slots track submitted producer values
separately from published consumer values. Failed signaling retains the source
and retries the same pending signal before another copy; unsuccessful ownership
handoff does not commit scheduler advancement. Keyed-mutex leases acquire with
zero timeout, publish key 1 on successful finish, and attempt to restore key 0
on failure. Before resize the provider clears history, tracked swapchain
references, and capture entries. Native D3D11 object destruction is deferred by
the runtime ([Microsoft documentation](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-flush));
it does not require D3D12-style allocator/resource retention. The existing WARP
failure fixture executes an unpublished copy, fails its fence signal, drops
capture ownership, flushes, and successfully resizes. The current hook-suite
rerun failed its hardware capture fixture: it expected width 48 but received
64 (`target/d3d11-retirement-audit.log`). This regression is under investigation;
the hook validation is not currently green. The failure also reproduces in
isolation. The fixture clears the backbuffer, copies it into the offscreen
source, then records a sampled draw. D3D11 currently has no clear-render-target
observer, so the copy predates its first observed target write. The corrected
temporal-provenance policy rejects that inference and falls back. Recording
the actual clear write, including deferred-context execution semantics, remains
required before this fixture can pass without weakening that policy.
Repeated native mutex-release failure is
not injected by this fixture and must not be claimed as hardware-verified.

D3D12 retirement audit: `D3d12SharedSlot` retains the selected source COM
resource along with output, allocator, and command list. Slot reuse requires
both producer completion and consumer release. Execution marks producer use
before queue signaling; signal failure leaves a pending value for retry and
does not demand acknowledgment for an unpublished output. `RetiredRing` takes
all slots plus queue/fence ownership and releases them only after completion;
if signaling/wait setup fails, it defers ownership in the shared retirement
queue. Before resize, capture history and tracking references are cleared and
capture-ring destruction waits for submitted producer work. The native resize
fixtures and failed-signal lease regression pass (13 producer, 3 hook tests).
Persistent native signal failure can retain source references and prevent a
successful resize; this is safe retention rather than proof of reclamation
under an unrecoverable device failure.

OpenGL target-evidence migration: the selection view now receives the shared
store's complete targets, retaining current-generation coverage scores and
stable observation order. Removed local target reconstruction from cached
resource metadata and the subsequent identity-only rendered filter. The
regression verifies that shared scores determine uncertain extent cohorts and
shared order sorts equal-size outputs, even when cached metadata disagrees.
All 23 OpenGL producer tests, the native hook test, and strict Clippy pass.

OpenGL presentation-observation migration: default-framebuffer sampled draws
now use the same stamped dependency recorder as offscreen draws. Removed the
separate texture-name presentation list and the selector adapter's generation-0
edge reconstruction. Known subresources are resolved at recording time, and
replay refreshes shared-store evidence. The recorder receives the already-known
context explicitly, so the regression needs no graphics context. All 22 OpenGL
producer tests pass. The rebuilt hook's graph-mode FFplay capture imports at
320x240 with 99 colors and is byte-identical to the documented whole-frame
reference (`target/ffplay-observation-graph.ppm`, matching test log).

Observation-store follow-up: a late exact copy could previously propagate
rendered-content provenance from a source write in a later generation. The
store now retains the copy edge but rejects that future provenance. A regression
failed before the fix and verifies that replay at the current generation can
still establish the destination. All 52 shared producer tests, the four graphics
producer suites, and strict shared Clippy passed after this correction. A
follow-up separates latest write order from stable first-observation order and
checks same-generation copies against it. The expanded regression and all four
graphics producer suites pass. Adapter ordering remains under audit: several
recording paths currently use the generation as their order, so this check
cannot distinguish commands for which the adapter supplies identical stamps.

Vulkan follow-up: submitted surface observations now receive monotonically
increasing order values while the observation transaction is held. All targets
and sampled edges of one draw share its order; transfers receive their own.
Replaying recorded commands refreshes order even without advancing the present
generation. The regression checks submission order, reversed replay, absence
before submission, and reset command invisibility. All 29 Vulkan tests and the
layer build check pass. This is host submission observation order, not proof of
GPU execution ordering between independently synchronized queues; that audit
and the other adapters' order values remain outstanding.

D3D follow-up: D3D11 commits and D3D12 submitted candidates now stamp target
and dependency evidence with monotonically increasing observation order.
Deferred/list recording alone does not allocate these stamps; execution and
replay do. Expanded tests verify same-generation replay refreshes dependency
order while stable target order survives. D3D11's four producer/seven hook tests
and D3D12's thirteen producer/three hook tests pass, including native unequal
streams and resize. OpenGL ordering and cross-queue execution semantics remain
under review.

OpenGL follow-up: offscreen draw targets and their sampled dependencies now
share a per-context observation order. Each copy/blit likewise stamps all its
layers or destinations with one order. These paths no longer substitute the
presentation generation for command order. All 21 OpenGL producer tests,
including native texture/renderbuffer captures through three transports, the
installed renderbuffer hook test, and strict OpenGL Clippy pass. Default-frame
presentation reconstruction and cross-API ordering consistency still need the
final selector/adapter audit.

Geometry follow-up: OpenGL viewport/scissor intersection and Vulkan raster
intersection now convert native rectangles and use the shared checked
`Rect::intersect`. Their duplicate arithmetic was removed. OpenGL's former
saturating endpoint addition could shorten otherwise representable intersections;
the adapter regression now covers that boundary. The 21 OpenGL and 29 Vulkan
tests, strict OpenGL Clippy, and both hook build checks pass.

Selector follow-up: removed its separate rendered-content propagation pass.
That pass lacked write timestamps and could recreate destination evidence that
the observation store had rejected as temporally invalid. `PresentationGraph`
now explicitly documents validated rendered-content targets as adapter/store
input. The end-to-end store-to-selector regression failed before removal; all
53 shared producer tests, all four graphics-provider suites, and strict shared
Clippy pass afterward. Explicit presented copy evidence remains supported.

Cycle follow-up: decomposition now deduplicates incoming candidates and the
expanded root set by strongly connected component, not only by surface ID.
Previously a composite could introduce two members of one cycle after initial
root deduplication, consuming two output slots. The regression reproduced this
and now yields one representative. Existing external-input cycle decomposition
still passes, together with all 54 shared producer tests and the four graphics
provider suites.

Whole-composite cycle follow-up: initial/whole-frame evidence is now associated
with the component rather than only its chosen surface representative. A
cropped representative can no longer discard a sibling member's explicit
whole-composite evidence and suppress required decomposition at the output
limit. The regression failed before the change and passes with the remaining
55 shared producer tests and all four graphics-provider suites. Strict shared
Clippy also passes.

Array-layer decomposition follow-up: the equal-or-larger scaling-chain stop
now counts distinct graph inputs after component and page-flip deduplication.
Counting resource identities incorrectly treated two independent array layers
as a single input and retained their composite. The regression reproduced that
failure and now selects both layers. All 56 shared producer tests, all four
graphics-provider suites, strict shared Clippy, and the CPU-only Linux build
pass.

Do not equate safe retention under native failure with completed reclamation.
Inspect the actual synchronization guarantees and distinguish required fixes
from unavailable hardware validation:

- OpenGL context deletion when neither a recorded DC nor recovery pbuffer can
  activate the original context. Current fallback disconnects and retains native
  resources to avoid destructors running in another context's namespace.
- Vulkan presentation semaphore lifetime without maintenance extensions,
  expanded/disjoint enqueue retries, and final device/swapchain destruction.
- GDI outstanding paint/DC ownership when release repeatedly fails or its owning
  GUI thread is unavailable.

Completion still requires a requirement-by-requirement final audit of the full
plan, removal of replaced policy paths, and current verification. No remaining
item is declared blocked merely because it needs further review.
