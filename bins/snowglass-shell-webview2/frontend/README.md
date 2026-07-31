# Snowglass WebView2 frontend

This React application owns the transparent foreground. It opts into the
native composed background with a document link:

```html
<link rel="background" href="/game-layer.html" />
```

The shell discovers and fetches that URL through the loaded foreground page,
then renders the linked document with Blitz. A page without the link has no
composed game layer.

The background document executes classic and module scripts in an embedded
Deno runtime. Relative and HTTP module imports, the standard Deno web APIs,
`fetch`, timers, and native DOM events are available. DOM mutations are applied
to the Blitz document before its next paint. `postMessage(value)` sends a
structured message to `window.gameLayer` in the foreground. The reverse
direction is `window.gameLayer.postMessage(value)`.

At startup the shell prints the loopback `/json` endpoint for the game-layer
inspector. Add that host and port under `chrome://inspect` to debug the
background realm with Chrome DevTools.

The foreground can access the background realm through the asynchronous
`contentWindow` proxy on either `window.gameLayer` or the background link:

```ts
const background = window.gameLayer.contentWindow;
const title = await background.document.title;
const surface = await background.document.querySelector(
  'tracked-surface[src="0"]',
);

// Browser-style postMessage is delivered as a message event in the background.
background.postMessage({ type: "foreground-ready" });
```

Property reads and calls cross WebView2 IPC, so their results are promises.
DOM objects, functions, and other platform objects remain live remote
references; JSON-like values are copied. Functions passed as arguments are
retained as callbacks, allowing operations such as
`background.addEventListener("event-name", callback)`.

Tracked surfaces attach to `HTMLTrackedSurfaceElement` objects in the
background realm:

```ts
{
  index: 0,
  attach: document.querySelector('tracked-surface[src="0"]'),
  rotation: 0,
  webrtc: false,
}
```

The background realm owns the canonical emulator API. Its active instance is
available synchronously to background scripts:

```ts
const instance = window.Emulator.activeInstance;
instance?.setVolume(0.5);
```

The foreground's `window.Emulator` is a proxy to the same namespace, and its
`activeInstance` is the same live object through the
background proxy, so reads and calls are asynchronous:

```ts
const instance = await window.Emulator.activeInstance;
if (instance) await instance.setVolume(0.5);
```

## Input API

`snowglassInput` is installed independently in the foreground and background
realms. Both objects reflect the same shell-owned, revisioned configuration:

```ts
snowglassInput.layouts;       // Stone controller layouts
snowglassInput.ports;         // physical source -> Stone virtual-port assignments
snowglassInput.getGamepads(); // Web Gamepad-shaped, layer-routed SDL snapshots

const updated = await snowglassInput.configure({
  // Omit this to use the revision currently cached by this realm.
  expectedRevision: snowglassInput.revision,
  operations: [{
    type: "setOverride",
    port: 0,
    target: "ButtonA",
    binding: {
      kind: "button",
      source: { kind: "gamepadButton", button: "east" },
    },
  }],
});
```

SDL discovery starts with the shell, so `getGamepads()` includes connected,
unassigned physical controllers before an emulator is launched. Each snapshot
has a connection-scoped `deviceId`, an SDL enumeration `index`, and an
`assignedPorts` array. Replacing the virtual-port topology does not restart
physical discovery.

While a game is running, the controller button in the bottom hotbar opens the
live remapping screen. Opening it transfers input priority to the foreground;
selecting a Stone control and pressing or moving a physical control applies a
revisioned override immediately. Defaults can be restored or targets disabled
from the same screen.

Transactions are atomic and stale revisions reject rather than overwriting a
newer foreground/background edit. `resetTarget` removes a user override and
restores the inherited mapping; a `{ kind: "disabled" }` override explicitly
leaves the Stone target unmapped.

The current SDL hook can apply SDL button/axis overrides while a game is
running. Source, port, or Stone-layout topology changes require the virtual
device to be recreated and are therefore rejected while an emulator is
attached. Keyboard and mouse-axis expressions are also compiled live:

```ts
{
  type: "setOverride",
  port: 0,
  target: "AxisRightAnalogPositiveX",
  binding: {
    kind: "axis",
    source: { kind: "mouseDelta", axis: "x" },
    transform: { scale: 1.5, deadZone: 0.02, invert: false },
  },
}
```

`mousePosition` uses the tracked surface's absolute `-1..1` coordinate.
`mouseDelta` uses raw device motion, treats 100 counts as full scale before the
binding transform, holds the latest sample for 16 ms, and decays it to neutral
by 50 ms. Direct pointer/touch delivery continues in parallel. Mouse-button
expressions remain represented but are not yet connected to the data plane.

Start the development server:

```powershell
npm install
npm run dev
```

Then pass the foreground entry URL to the shell.

```powershell
cargo run --bin snowglass-shell-webview2 -- `
  --url http://127.0.0.1:5173/shell.html `
  --track-render-targets "0:0:Cemu 2\.6+,1:0:GamePad+" `
  --gamepad K:0:WIIU_DRC_CONTROLLER `
  --api d3d12 `
  -- E:\Emulators\Cemu\Cemu.exe
```

