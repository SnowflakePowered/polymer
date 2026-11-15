import solid from "vite-plugin-solid";
import { defineConfig } from "vite";

export default defineConfig({
  base: "./",
  plugins: [solid()],
  resolve: { dedupe: ["solid-js"] },
  build: { modulePreload: { polyfill: false } },
});
