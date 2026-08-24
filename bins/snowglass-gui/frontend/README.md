# Snowglass Solid frontend

This directory contains the shell's single Solid 1 application. It runs in the
native Deno/Blitz document; it is not a browser or WebView frontend.

Install and build:

```powershell
npm install
npm run build
```

For development, start Vite and pass its one document to the shell:

```powershell
npm run dev
cargo run --bin snowglass-shell-webview2 -- --url http://127.0.0.1:5173/index.html
```

For a built frontend, pass an absolute `file:` URL for `dist/index.html`.
Assets are intentionally not embedded in the executable.

The app uses `Snowglass.scraping`, `Snowglass.configuration`,
`Snowglass.emulator`, `Snowglass.webrtc`, `Snowglass.input`, and the
`HTMLTrackedSurfaceElement` DOM interface. Solid Router uses memory history,
and the persistent `/game` parent retains tracked-surface identity across
launching, play, and remapping. Ark UI is headless and Tailwind emits static
CSS.

Ark 5.38 and the shell share the single pinned Solid 1 runtime directly; no
renderer aliases or compatibility adapters are used.
