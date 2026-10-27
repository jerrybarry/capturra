import { resolve } from "node:path";
import { defineConfig } from "vite";

// Tauri expects a fixed port and does its own screen clearing. Three pages: the menu-bar popover
// (index.html), the Studio window (pages/studio.html) and the region picker (pages/region.html).
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: "safari15",
    outDir: "dist",
    rollupOptions: {
      input: { main: resolve(__dirname, "index.html"), studio: resolve(__dirname, "pages/studio.html"), region: resolve(__dirname, "pages/region.html") },
    },
  },
});
