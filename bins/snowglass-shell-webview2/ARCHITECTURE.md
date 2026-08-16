# Snowglass Solid/Blitz shell

The crate keeps its historical `snowglass-shell-webview2` name, but it no
longer creates a WebView or Chromium process. One Solid application executes in
one Deno UI isolate and mirrors its DOM into one Blitz document.

## Startup

`--url` identifies the only UI document. The shell creates the native Winit
window, fetches that URL (including `file:` URLs), parses it with `blitz-html`,
starts the Deno runtime, executes the document's module script, applies queued
DOM mutations, resolves layout, and paints the first frame.

`blitz-net` is the resource provider for HTTP, file, data, stylesheets, images,
and fonts. Its waker posts a shell redraw event when a resource arrives.

## UI ownership

- `ShellDocument` owns Blitz parsing, Stylo layout, the tracked-surface custom
  widget, banded painting, hit testing, and script/DOM identity maps.
- `ShellRuntime` owns the sole Deno UI realm and the browser compatibility
  wrappers used by Solid and Ark UI.
- `ShellDomCoordinator` pairs those objects and publishes immutable paint and
  layout snapshots to the compositor.
- `ShellLaunchCoordinator` validates launcher state and calls pluginhost
  directly. Plugin invocation workers remain separate from the UI isolate.
- `WindowManager` owns the Winit window, merged shell/game DirectComposition
  visual, and the egui diagnostics overlay.

There is no foreground proxy, linked document, shared UI bus, or structured
remote-value codec. `Emulator`, `snowglassInput`, `snowglassLauncher`, and
`HTMLTrackedSurfaceElement` are synchronous objects in the Solid realm.

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

The frontend has one `index.html` and one `src/main.tsx`. Vite builds Solid 1,
Solid Router memory history, Ark UI, and Tailwind into static JavaScript and
CSS. Development uses `http://127.0.0.1:5173/index.html`; a built tree uses a
`file:` URL pointing at `frontend/dist/index.html`.
