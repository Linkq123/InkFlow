import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  resolve: {
    conditions: ["browser"],
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  worker: {
    format: "es",
  },
  build: {
    target: ["es2022", "chrome105"],
    minify: "esbuild",
    sourcemap: false,
    // Mermaid's parser is an optional, upstream single-module chunk (~669 kB).
    // Keep the warning focused on regressions above that known lazy boundary.
    chunkSizeWarningLimit: 690,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
