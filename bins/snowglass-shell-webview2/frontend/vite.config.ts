import { fileURLToPath, URL } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  base: "./",
  plugins: [react()],
  build: {
    modulePreload: { polyfill: false },
    rollupOptions: {
      input: {
        shell: fileURLToPath(new URL("./shell.html", import.meta.url)),
        gameLayer: fileURLToPath(new URL("./game-layer.html", import.meta.url)),
      },
    },
  },
});
