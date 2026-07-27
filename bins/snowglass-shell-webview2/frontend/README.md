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

