# Polymer Solid/Blitz shell

The crate keeps its historical `polymer-shell-webview2` name, but it no
longer creates a WebView or Chromium process. One Solid application executes in
one Deno UI isolate and mirrors its DOM into one Blitz document.

## Startup

The selected `theme` plugin identifies the only UI document. Selection uses a
temporary `--theme` override, then `localStorage["Polymer.theme"]`, then the
first loaded theme by ID. The shell creates the native Winit window, loads the
theme document, parses it with `blitz-html`,
starts the Deno runtime, executes the document's module script, applies queued
DOM mutations, resolves layout, and paints the first frame.

`--debug` starts the selected theme's `[theme.debug].serve` command from its
package directory, waits for `[theme.debug].url`, and keeps that process alive
for the shell lifetime. Normal startup loads `[theme].main` as a confined
`file:` URL. The Deno inspector remains available in either mode.

`blitz-net` is the resource provider for HTTP, file, data, stylesheets, images,
and fonts. Its waker posts a shell redraw event when a resource arrives.

## UI ownership

- `ShellDocument` owns Blitz parsing, Stylo layout, the tracked-surface custom
  widget, banded painting, hit testing, and script/DOM identity maps.
- `ShellRuntime` owns the sole Deno UI realm and the DOM compatibility wrappers
  used by Solid and Ark UI. The optional `polymer-deno` extension owns the
  non-DOM `Polymer` facades, their ops, and a typed notification inbox.
- `ShellDomCoordinator` pairs those objects and publishes immutable paint and
  layout snapshots to the compositor.
- `Polymer.scraping.Package` delegates package identification to Gumshoe in
  `polymer-runtime`; `Polymer.configuration` describes and validates launch
  selections. Plugin invocation workers remain isolated from the UI realm.
- `WindowManager` owns the Winit window, merged shell/game DirectComposition
  visual, and the egui diagnostics overlay.

## Runtime crate boundaries

- `polymer-shell-webview2` constructs the application Deno isolate and owns
  DOM adaptation, window input, capture composition, and other native
  presentation resources. It does not implement `Polymer.*` services.
- `polymer-deno` defines the curated web runtime, the frontend-only
  `Polymer.*` JavaScript extension, its ops, notifications, and the typed
  `PolymerHost` capability boundary. Its ops contain input conversion and
  delegation only.
- `polymer-runtime` implements `PolymerHost`. It owns configuration and
  console-storage services, package scraping, plugin dispatch, prepared VFS
  lifetime, filesystem mounting, and emulator process/audio/session lifecycle.
  `RuntimePlatform` is the narrow port for operations that must return to the
  shell's native event thread.
- `polymer-pluginhost` owns plugin discovery and invocation, plugin Deno
  isolates, and plugin-only JavaScript APIs such as filesystem preparation.
  A prepared filesystem is returned as an owned Rust value to
  `polymer-runtime`; plugin isolates never receive frontend `Polymer.*`
  APIs and do not retain application session handles.

The dependency direction is shell -> runtime -> pluginhost and deno, with the
shell also depending on deno to instantiate its UI isolate. Neither deno nor
pluginhost depends on shell types.

There is no foreground proxy, linked document, shared UI bus, or structured
remote-value codec. `Polymer.input`, `Polymer.plugin`, `Polymer.scraping`,
`Polymer.configuration`, `Polymer.emulator`, and `Polymer.webrtc` are
same-realm APIs. `Polymer.emulator.launch()` synchronously returns a
`LaunchInstance`; its promises and events own pending, ready, error, and closed
lifecycle state. Emulator launch, WebRTC publication control, and plugin
invocation cross asynchronous Deno op boundaries. Deno owns WebRTC lifecycle
and launch association; `polymer-renderer` keeps signaling, peer sessions,
GPU composition, encoding, and frame publication together in the native data
plane.

Native feature producers send owned `PolymerNotification` values directly
to the inbox. A single pending Deno op feeds those values to the JavaScript
facades, so adding a notification does not add forwarding methods to
`ShellDomCoordinator`, `ShellRuntime`, or its Deno runtime.

The base `polymer-deno::Runtime` does not install this optional extension.
Plugin workers therefore keep their isolated web platform and never receive
the application-facing `Polymer` global.

## DOM and events

Deno supplies web events, timers, promises, fetch, WebSockets, URL, storage,
streams, and structured clone. Blitz supplies DOM/CSS/layout/painting and
native default actions. The bridge preserves JavaScript node identity, queues
typed mutations, synchronizes post-layout geometry, and converts Blitz events
to Deno event subclasses.

Pointer input is hit-tested against Blitz first and dispatched through Blitz's
`EventDriver`. Script cancellation updates Blitz `EventState`, so native
defaults run only when allowed. An unclaimed event over a `tracked-surface`
then falls through to the guest. The egui diagnostics overlay remains highest
priority. Keyboard, wheel, focus, generated input, and Winit IME events use the
same driver.

The Blitz `ShellProvider` posts redraw/cursor/title/IME commands to Winit and
uses the process clipboard through `arboard`.

## Composition

Blitz paints ordinary UI and chrome into retained Vello scenes. The
tracked-surface widget records game placements rather than painting captured
frames into the UI scene. The banded compositor interleaves those scenes with
captured game textures in one opaque merged visual; egui is a separate
diagnostic overlay visual.

The `/game` Solid route is persistent. Its tracked-surface elements retain
identity while child routes move between launching, play, and remapping.

## Frontend

The built-in Chipotle frontend lives in `plugins/theme/chipotle`, with one
`index.html` and one `src/main.tsx`. Vite builds Solid 1, Solid Router memory
history, Ark UI, and Tailwind into static JavaScript and CSS. Its development
document is `http://127.0.0.1:5173`; its built document is
`dist/index.html` inside the plugin package.
