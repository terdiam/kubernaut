/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The frontend lives in `ui/` while Tauri owns `src-tauri/`; keeping Vite's
// root there avoids a second package.json.
export default defineConfig({
  root: "ui",
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    // Tauri surfaces build errors better when Vite fails loudly.
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  // Component tests need a DOM; the pure helpers run fine in it too.
  test: {
    environment: "jsdom",
    include: ["ui/src/**/*.test.{ts,tsx}"],
    root: ".",
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: "es2022",
    sourcemap: true,
    // Monaco and xterm are large but load from disk in the packaged app, so the
    // warning is noise. Splitting them still helps first paint.
    chunkSizeWarningLimit: 4096,
    rollupOptions: {
      output: {
        manualChunks: {
          xterm: ["@xterm/xterm", "@xterm/addon-fit", "@xterm/addon-web-links"],
        },
      },
    },
  },
});
