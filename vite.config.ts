import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const workerSafeCharacterDecoder = require.resolve(
  "decode-named-character-reference",
);
const workerSafeHtmlParser = require.resolve("hast-util-from-html-isomorphic");

export default defineConfig({
  plugins: [svelte()],
  resolve: {
    // The browser export reads `document`, but this dependency is also used by
    // the Markdown Web Worker. Its default implementation is DOM-free and has
    // identical behavior in the WebView main thread.
    alias: [
      {
        find: /^decode-named-character-reference$/,
        replacement: workerSafeCharacterDecoder,
      },
      {
        find: /^hast-util-from-html-isomorphic$/,
        replacement: workerSafeHtmlParser,
      },
    ],
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
    rollupOptions: {
      input: ["index.html", "renderer.html"],
    },
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
