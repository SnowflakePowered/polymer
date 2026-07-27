# Snowglass WebView2 frontend

This React application is independent from the native shell. Vite serves the
transparent foreground UI from `shell.html`; it owns the
`<game-view-target>` elements used to attach captured surfaces.

The native shell renders `src/game-layer.html` with Blitz and composites its
background, chrome, and captured game surfaces into one swapchain. That layer
is selected by default; pass `--game-layer <HTML>` to use another document.

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

`src/gameview.ts` contains frontend-only type declarations and the CLI launch
subscription helper. The executable injects the runtime bridge separately
before the page loads.
