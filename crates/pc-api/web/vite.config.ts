import { defineConfig } from "vite";
export default defineConfig({
  build: {
    assetsDir: "static",
    rollupOptions: {
      output: {
        entryFileNames: "static/app.js",
        assetFileNames: "static/app.[ext]",
      },
    },
  },
  server: {
    proxy: {
      // changeOrigin rewrites the Host header to match the target so a
      // loopback-bound pc-api's DNS-rebinding guard (crates/pc-api/src/
      // security.rs) doesn't see `Host: localhost:5173` and reject every
      // /api call with 421 during `npm run dev`.
      "/api": { target: "http://127.0.0.1:8080", changeOrigin: true },
    },
  },
});
