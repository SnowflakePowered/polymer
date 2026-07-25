import { fileURLToPath, URL } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  build: {
    rollupOptions: {
      input: {
        shell: fileURLToPath(new URL("./shell.html", import.meta.url)),
        back: fileURLToPath(new URL("./back.html", import.meta.url)),
      },
    },
  },
});
