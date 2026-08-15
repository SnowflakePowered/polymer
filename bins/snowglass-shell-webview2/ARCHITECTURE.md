# Snowglass WebView2 shell boundaries

The shell is coordinated from the application event loop, but ownership is
split along the following boundaries:

- `background_dom.rs` owns activation and snapshots for the Blitz-backed
  document. It publishes immutable `BackgroundFrame`, `TrackedSurfaceLayout`,
  and `BackgroundHitSnapshot` values; `background_document.rs` owns the
  document and produces those snapshots. Plugin packages are never installed
  into this isolate. GPU rasterization and shared-texture allocation are owned
  by `game_host.rs`.
- `background_runtime.rs` owns the long-lived browser-like runtime for
  background document scripts. `BackgroundDocument` contains no isolate or plugin
  state. Its `BackgroundLaunchCoordinator` also owns launcher snapshots, game
  detection generations, prepared games, and plugin-backed launch-plan
  realization. The runtime's `snowglass.plugins` facade writes to the shared bus;
  the native broker calls the cloneable pluginhost runtime handle, so each
  actual plugin call runs elsewhere in a fresh isolate. DOM bindings remain in
  `runtime.js`; event dispatch, input, and emulator lifecycle facades are
  separate modules installed before that bootstrap. Both DOM bridges configure
  the same live-reference envelope codec instead of duplicating traversal and
  callback/reference handling.
- `foreground_dom.rs` owns foreground WebView2 construction, navigation,
  controller bounds, focus, mouse injection, and parent-window synchronization.
  It also owns the single WebView2 shared-buffer allocation, its foreground
  attach/release lifecycle, and foreground doorbells. It consumes an opaque
  `SurfaceWindow` and does not import `winit`.
- `game_host.rs` owns the active emulator process, capture and pointer
  transports, WebRTC publisher, audio state, and tracked native surfaces.
  It also constructs and replaces the merged D3D12 compositor, consumes raw
  capture offers/frames/removals, and publishes only neutral capture lifecycle
  results plus optional visual-slot content to the coordinator.
  Plugin-generated files remain inert data on the launch plan until this
  boundary validates the config root, installs the files, converts file URLs,
  rotations, hook backends, and WebRTC addresses, and spawns the process.
  It converts an `AttachedLaunchPlan` into the runtime's native process/hook/
  capture configuration; those platform conversions do not live in `app.rs`.
  It also owns the Vello rasterizer, merged compositor, capture event
  consumption, stream replacement, pointer presentation, and target snapshots.
  `app.rs` applies only the resulting window/composition and DOM lifecycle
  effects.
- `launch.rs` defines the native game-host launch boundary. CLI and plugin
  code produce `UnattachedLaunchPlan`; only a DOM-resolved
  `AttachedLaunchPlan` may reach emulator process/capture setup.
- `input.rs` owns device selectors, gamepad mappings, binding parsing, the
  replaceable IPC input transport, foreground hit snapshots, pointer routing
  state, capture/focus priority, physical-device state, and the complete
  launch-time hub/provider configuration transaction. Its mutable routing
  state is private. Game hosts expose opaque input endpoints, and foreground
  WebView injection consumes neutral `DomPointerEvent` values whose Win32
  translation stays in `foreground_dom.rs`.
- `window_manager.rs` is the Win32/DirectComposition adapter with named
  foreground, game, and overlay visual slots. Background paint
  and captured game textures are merged before their content reaches its
  visual; WebView2 remains in the foreground visual. Its `WindowManager` owns
  the shell window, composition root, frame pacing, and egui overlay. Its
  `EventLoopApplication` is the only raw winit handler and translates events
  into neutral shell values before calling `app.rs`.
- `game_host` owns one `RenderDevice` for the active DXGI adapter. Its Vello
  renderer, native chrome textures, D3D12 compositor, swapchain, and imported
  hook textures all use the same D3D12 device and direct queue. Chrome crosses
  the wgpu/compositor crate-version boundary as a retained COM reference, not
  a shared handle. Hook textures remain foreign shared resources. An offer
  from another adapter transactionally constructs and installs a complete
  replacement `RenderDevice`, then invalidates chrome for repaint.
- `snowglass-pluginhost` separates `PluginRegistry` from `PluginRuntime` and
  `PluginRuntimeHandle`. It also owns the shared structured-value codec and
  four-lane FIFO protocol. Windows/WebView2 mapping and V8 external-memory
  attachment remain shell adapter concerns so pluginhost stays safe Rust.

## Plugin invocation contract

`snowglass.plugins.open(id)` returns a lazy callable proxy. Property access
only records an export path. Calling that path returns a Promise and schedules
a fresh curated Deno/V8 isolate. Resolution prefers a matching named export,
falls back to the same property on the default object, and supports an
explicit `default` path.

`setupLaunch` is special only at the native resource boundary: pluginhost
materializes an invocation-scoped configuration from `console` and
`portAssignments`, injects its proxy, realizes generated files, and destroys
the proxy with the worker. No isolate-owned object crosses back into either
DOM.

## Shared bus protocol

The platform-neutral bus core is in `snowglass-pluginhost::bus`. It defines a
versioned header, non-zero request correlation, generation reset, four
independent SPSC lanes, asynchronous full-lane backpressure, release/acquire
publication, and fail-closed cursor/record validation. Its binary codec
supports cyclic arrays and plain objects, maps, sets, dates, errors, bigint,
ArrayBuffer, and typed arrays without JSON or base64 conversion.

`shared_bus.rs` implements the mapped native lane adapter; `ForegroundDom`
owns its one `CoreWebView2SharedBuffer`. WebView2 receives the allocation as
its shared `ArrayBuffer`; the
background Deno isolate receives the same address as an external
`SharedArrayBuffer`, with a V8 backing-store deleter retaining the COM owner.
Foreground and background doorbells contain no application payload. Plugin
requests, settlements, ordinary DOM messages, remote-object operations, and
foreground shell-control commands are binary FIFO records in lanes 1/2 and
3/4, respectively. The broker forwards DOM-targeted records without decoding
their payload. Navigation increments the generation, cancels stale calls,
releases the old background runtime, and reattaches the shared allocation to
the new foreground document.

The shared JavaScript codec source is owned by `snowglass-pluginhost` and is
installed in both DOMs and every fresh plugin worker. Generic plugin arguments
and results remain encoded through worker invocation and settlement, preserving
cycles and rich structured-clone types without a serde/JSON intermediate.
`setupLaunch` alone is converted to its deliberately JSON-shaped Rust launch
contract so pluginhost can validate and materialize configuration resources.

The cross-realm value codec remains an explicit wire format instead of using a
Rust-only serializer such as [`bitcode`](https://docs.rs/bitcode/latest/bitcode/).
Both JavaScript realms must encode and
decode it directly, and the protocol needs a stable versioned format with
fail-closed tag/length/reference validation. A WASM wrapper around a Rust-only
codec would still require the same JavaScript object-graph traversal while
adding copies at the WASM boundary.
