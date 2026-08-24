import solid from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

export default defineConfig({
  base: "./",
  plugins: [solid(), tailwindcss()],
  resolve: {
    dedupe: ["solid-js"],
  },
  build: { modulePreload: { polyfill: false } },
});
