# Chipotle theme

Chipotle is the built-in Solid 1 theme for the native Polymer Deno/Blitz
shell. Install its frontend dependencies once:

```powershell
npm install
```

For development, Polymer starts Vite from this package automatically and
loads the configured development URL:

```powershell
cargo run --bin polymer-gui -- --debug
```

Port 5173 must be free so Polymer can own and clean up the configured Vite
process.

For a normal run, build the production document first:

```powershell
npm run build
cargo run --bin polymer-gui
```

The selected theme is read from `localStorage["Polymer.theme"]`. Passing
`--theme chipotle` overrides that value for one run without changing it.

The app uses `Polymer.scraping`, `Polymer.configuration`,
`Polymer.emulator`, `Polymer.webrtc`, `Polymer.input`, and the
`HTMLTrackedSurfaceElement` DOM interface. Solid Router uses memory history,
and the persistent `/game` parent retains tracked-surface identity across
launching, play, and remapping.
