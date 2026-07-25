# Snowglass WebView2 frontend

This React application is independent from the native shell. Vite serves two
entry points:

- `shell.html` is the transparent foreground UI and owns all
  `<game-view-target>` elements.
- `back.html` is the background UI rendered below native game surfaces.

Start the development server:

```powershell
npm install
npm run dev
```

Then pass the foreground entry URL to the shell. The native shell derives the
background URL by replacing the final path segment with `back.html`.

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
