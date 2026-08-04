import { fileURLToPath } from "node:url";

import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// Backend to proxy /api calls to while running the frontend dev server.
const API_TARGET = process.env.KWP_API_URL ?? "http://localhost:8080";

export default defineConfig({
  plugins: [svelte(), tailwindcss()],

  resolve: {
    alias: {
      $lib: fileURLToPath(new URL("./src/lib", import.meta.url)),
    },
  },

  build: {
    // Embedded into the binary by rust_embed (src/bin/kwp/static_files.rs).
    outDir: "../static",
    emptyOutDir: true,
  },

  server: {
    port: 4200,
    allowedHosts: true,
    proxy: {
      "/api": {
        target: API_TARGET,
        changeOrigin: true,
      },
    },
  },
});
